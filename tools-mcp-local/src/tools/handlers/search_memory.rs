//! In-memory fast path for the `Search` tool.

use super::search_contract::{
    NormalizedSearchRequest, RenderedSearchEvent, SearchCaseMode, SearchEvent, SearchPayloadMeta,
    SearchRequest, build_search_payload_from_rendered, render_search_events,
    render_search_text_capacity_from_rendered,
};
use super::search_file_selection::{FileSelectionError, FileSelector};
use crate::tools::scope_cache::{IgnoreFingerprint, ignore_fingerprint_change_reason};
use memchr::{memchr, memchr_iter, memchr2};
use regex::bytes::{Regex, RegexBuilder};
use regex_syntax::{
    ParserBuilder as RegexParserBuilder,
    hir::{Class, Hir, HirKind},
};
use serde_json::{Value, json};
use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::cancellation::current_cancellation_token;
use tracing::{debug, info, warn};

const DEFAULT_MAX_FILE_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_FILES: usize = 50_000;
const DEFAULT_MAX_CANDIDATES: usize = 20_000;
const DEFAULT_MAX_FUZZY_PATTERN_CHARS: usize = 512;
const DEFAULT_MAX_FUZZY_VERIFIED_LINES: usize = 200_000;
const DEFAULT_MAX_FUZZY_LINE_CHARS: usize = 16_384;
const DEFAULT_MAX_SHORT_LITERAL_SCAN_LINES: usize = 200_000;
const DEFAULT_REGEX_SIZE_LIMIT_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_WARM_CACHE_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_WARM_CACHE_START_DELAY_MS: u64 = 250;
const DEFAULT_WARM_CACHE_KEY_DELAY_MS: u64 = 25;
const DEFAULT_WARM_CACHE_MAX_KEYS: usize = 6;
const DEFAULT_WARM_CACHE_GLOBS: &str = "*.rs,*.md";
const DEFAULT_WARM_CACHE_GIT_TIMEOUT_MS: u64 = 2_000;
const TRIGRAM_DEADLINE_CHECK_STRIDE: usize = 1024;
const POSTINGS_DEADLINE_CHECK_STRIDE: usize = 1024;
const LINE_VERIFY_DEADLINE_CHECK_STRIDE: usize = 128;
const MAX_REGEX_FINITE_LITERAL_ALTERNATIVES: usize = 64;
const MAX_REGEX_FINITE_LITERAL_BYTES: usize = 64;
const MAX_REGEX_FINITE_LITERAL_REPEAT_COUNT: usize = 8;
const MAX_FUZZY_SEED_PARTITION_PLANS: usize = 128;
const TARGETED_FRESHNESS_FULL_SCAN_INTERVAL_QUERIES: u64 = 32;
const WARM_CACHE_PATTERN: &str = "__tools_mcp_search_warm_cache__";

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

    fn cancelled() -> Self {
        Self {
            error_type: "cancelled",
            fallback_reason: "cancelled",
            fallback_allowed: false,
            message: "memory search cancelled".to_string(),
            timed_out: false,
        }
    }

    fn is_per_request_failure(&self) -> bool {
        self.timed_out || matches!(self.error_type, "cancelled" | "query_timeout")
    }

    pub(super) fn into_tool_outcome(self, req: &NormalizedSearchRequest) -> ToolCallOutcome {
        ToolCallOutcome::err_with(
            self.message,
            [
                ("backend", json!("memory")),
                ("error_type", json!(self.error_type)),
                ("fallback_reason", json!(self.fallback_reason)),
                ("fallback_available", json!(self.fallback_allowed)),
                ("memory_eligibility", json!("error")),
                (
                    "remediation",
                    json!(
                        "Use a narrower fixed-string search, reduce the search scope, or retry with a larger timeout."
                    ),
                ),
                ("pattern", json!(req.pattern())),
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

impl From<FileSelectionError> for MemoryError {
    fn from(err: FileSelectionError) -> Self {
        if err.timed_out {
            return Self::timeout();
        }

        Self {
            error_type: err.error_type,
            fallback_reason: err.fallback_reason,
            fallback_allowed: err.fallback_allowed,
            message: err.message,
            timed_out: false,
        }
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
    max_short_literal_scan_lines: usize,
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
            max_short_literal_scan_lines: env_usize(
                "TOOLS_SEARCH_MAX_SHORT_LITERAL_SCAN_LINES",
                DEFAULT_MAX_SHORT_LITERAL_SCAN_LINES,
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
}

#[derive(Clone, Debug)]
struct Document {
    path: PathBuf,
    rendered_path: String,
    stamp: FileStamp,
    content: Vec<u8>,
    lines: Vec<LineRange>,
}

#[derive(Clone, Debug, Default)]
struct ScopeFingerprint {
    directories: Vec<MetadataFingerprint>,
}

#[derive(Clone, Debug)]
struct MetadataFingerprint {
    path: PathBuf,
    stamp: FileStamp,
}

impl ScopeFingerprint {
    fn from_directories<I>(directories: I, deadline: Instant) -> Result<Self, MemoryError>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut fingerprints = Vec::new();
        for path in directories {
            check_deadline(deadline)?;
            let metadata = fs::metadata(&path).map_err(|err| {
                MemoryError::new(
                    "search_index_incomplete",
                    "metadata_error",
                    format!(
                        "failed to read directory metadata for {}: {err}",
                        path.display()
                    ),
                )
            })?;
            fingerprints.push(MetadataFingerprint {
                path,
                stamp: metadata_stamp_from_metadata(&metadata),
            });
        }
        check_deadline(deadline)?;
        Ok(Self {
            directories: fingerprints,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LiteralCase {
    Sensitive,
    AsciiInsensitive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct DocId(u32);

impl DocId {
    fn from_index(index: usize) -> Result<Self, MemoryError> {
        let value = u32::try_from(index).map_err(|_| {
            MemoryError::new(
                "resource_limit_exceeded",
                "doc_id_width_exceeded",
                "memory search document count exceeds doc-id representation",
            )
        })?;
        Ok(Self(value))
    }

    fn to_index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DocumentFrequency(usize);

impl DocumentFrequency {
    fn get(self) -> usize {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PostingsStorage {
    Empty,
    Inline(DocId),
    Many(Vec<DocId>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Postings {
    storage: PostingsStorage,
    document_frequency: DocumentFrequency,
}

impl Postings {
    fn from_doc_ids(mut doc_ids: Vec<DocId>, deadline: Instant) -> Result<Self, MemoryError> {
        check_deadline(deadline)?;
        doc_ids.sort_unstable();
        check_deadline(deadline)?;
        doc_ids.dedup();

        let document_frequency = DocumentFrequency(doc_ids.len());
        let storage = match doc_ids.len() {
            0 => PostingsStorage::Empty,
            1 => PostingsStorage::Inline(doc_ids[0]),
            _ => PostingsStorage::Many(doc_ids),
        };

        Ok(Self {
            storage,
            document_frequency,
        })
    }

    fn document_frequency(&self) -> DocumentFrequency {
        self.document_frequency
    }

    fn len(&self) -> usize {
        self.document_frequency.get()
    }

    fn doc_id_at(&self, index: usize) -> DocId {
        match &self.storage {
            PostingsStorage::Empty => unreachable!("empty postings have no document ids"),
            PostingsStorage::Inline(doc_id) => {
                debug_assert_eq!(index, 0);
                *doc_id
            }
            PostingsStorage::Many(doc_ids) => doc_ids[index],
        }
    }

    fn to_vec(&self) -> Vec<DocId> {
        match &self.storage {
            PostingsStorage::Empty => Vec::new(),
            PostingsStorage::Inline(doc_id) => vec![*doc_id],
            PostingsStorage::Many(doc_ids) => doc_ids.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct PostingsIndex {
    entries: HashMap<[u8; 3], Postings>,
}

impl PostingsIndex {
    fn from_raw(
        raw_entries: HashMap<[u8; 3], Vec<DocId>>,
        deadline: Instant,
    ) -> Result<Self, MemoryError> {
        let mut entries = HashMap::with_capacity(raw_entries.len());
        for (trigram, doc_ids) in raw_entries {
            check_deadline(deadline)?;
            let postings = Postings::from_doc_ids(doc_ids, deadline)?;
            if postings.document_frequency().get() > 0 {
                entries.insert(trigram, postings);
            }
        }

        Ok(Self { entries })
    }

    fn get(&self, trigram: &[u8; 3]) -> Option<&Postings> {
        self.entries.get(trigram)
    }
}

#[derive(Debug)]
struct IndexSnapshot {
    generation: u64,
    documents: Vec<Document>,
    scope_fingerprint: ScopeFingerprint,
    ignore_fingerprint: Option<IgnoreFingerprint>,
    postings: PostingsIndex,
    ascii_folded_postings: PostingsIndex,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexEntryState {
    #[allow(dead_code)]
    Disabled,
    Cold,
    Building,
    Ready,
    Refreshing,
    Unavailable,
}

impl IndexEntryState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Cold => "cold",
            Self::Building => "building",
            Self::Ready => "ready",
            Self::Refreshing => "refreshing",
            Self::Unavailable => "unavailable",
        }
    }

    fn has_usable_snapshot(self) -> bool {
        matches!(self, Self::Ready | Self::Refreshing)
    }
}

#[derive(Debug)]
struct IndexEntry {
    state: IndexEntryState,
    snapshot: Option<Arc<IndexSnapshot>>,
    generation: Option<u64>,
    last_error: Option<MemoryError>,
    last_error_type: Option<&'static str>,
    last_fallback_reason: Option<&'static str>,
    queries_since_full_validation: u64,
}

impl IndexEntry {
    fn cold() -> Self {
        Self {
            state: IndexEntryState::Cold,
            snapshot: None,
            generation: None,
            last_error: None,
            last_error_type: None,
            last_fallback_reason: None,
            queries_since_full_validation: 0,
        }
    }

    #[cfg(test)]
    fn ready(snapshot: Arc<IndexSnapshot>) -> Self {
        Self {
            state: IndexEntryState::Ready,
            generation: Some(snapshot.generation),
            snapshot: Some(snapshot),
            last_error: None,
            last_error_type: None,
            last_fallback_reason: None,
            queries_since_full_validation: 0,
        }
    }

    fn begin_build(&mut self, generation: u64) {
        self.state = IndexEntryState::Building;
        self.snapshot = None;
        self.generation = Some(generation);
        self.last_error = None;
        self.last_error_type = None;
        self.last_fallback_reason = None;
        self.queries_since_full_validation = 0;
    }

    fn publish(&mut self, snapshot: Arc<IndexSnapshot>) {
        self.state = IndexEntryState::Ready;
        self.generation = Some(snapshot.generation);
        self.snapshot = Some(snapshot);
        self.last_error = None;
        self.last_error_type = None;
        self.last_fallback_reason = None;
        self.queries_since_full_validation = 0;
    }

    fn usable_snapshot(&self) -> Option<Arc<IndexSnapshot>> {
        self.state
            .has_usable_snapshot()
            .then(|| self.snapshot.clone())
            .flatten()
    }

    fn has_ready_snapshot(&self) -> bool {
        self.snapshot.is_some() && self.state.has_usable_snapshot()
    }

    fn current_snapshot_is(&self, snapshot: &Arc<IndexSnapshot>) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, snapshot))
    }

    fn mark_refreshing(&mut self) {
        if self.has_ready_snapshot() {
            self.state = IndexEntryState::Refreshing;
        }
    }

    fn full_validation_due(&self) -> bool {
        self.queries_since_full_validation >= TARGETED_FRESHNESS_FULL_SCAN_INTERVAL_QUERIES
    }

    fn mark_ready_after_validation(&mut self, snapshot: &Arc<IndexSnapshot>, full_scope_ran: bool) {
        if self.current_snapshot_is(snapshot) {
            self.state = IndexEntryState::Ready;
            if full_scope_ran {
                self.queries_since_full_validation = 0;
            } else {
                self.queries_since_full_validation =
                    self.queries_since_full_validation.saturating_add(1);
            }
        }
    }

    fn mark_unavailable(&mut self, err: &MemoryError) {
        self.state = IndexEntryState::Unavailable;
        self.snapshot = None;
        self.last_error = Some(err.clone());
        self.last_error_type = Some(err.error_type);
        self.last_fallback_reason = Some(err.fallback_reason);
        self.queries_since_full_validation = 0;
    }

    fn clear_failed_build(&mut self) {
        self.state = IndexEntryState::Cold;
        self.snapshot = None;
        self.generation = None;
        self.last_error = None;
        self.last_error_type = None;
        self.last_fallback_reason = None;
        self.queries_since_full_validation = 0;
    }
}

#[derive(Debug)]
struct SharedIndexManager {
    manager: Mutex<IndexManager>,
}

impl Default for SharedIndexManager {
    fn default() -> Self {
        Self {
            manager: Mutex::new(IndexManager::default()),
        }
    }
}

impl SharedIndexManager {
    fn lock(&self) -> MutexGuard<'_, IndexManager> {
        self.manager.lock().unwrap_or_else(|poisoned| {
            warn!("search index manager mutex was poisoned; recovering in-memory state");
            poisoned.into_inner()
        })
    }

    /// Wait on the per-key condvar associated with `key`, releasing the manager guard
    /// while waiting. Returns the re-acquired guard and whether the wait timed out.
    ///
    /// If no per-key condvar is registered for `key` (e.g. the build completed between
    /// the caller's lookup and this call), returns immediately with `timed_out=false`
    /// so the caller can re-check the cache.
    fn wait_for_build<'a>(
        &self,
        guard: MutexGuard<'a, IndexManager>,
        key: &IndexKey,
        timeout: Duration,
    ) -> (MutexGuard<'a, IndexManager>, bool) {
        let Some(condvar) = guard.in_progress.get(key).cloned() else {
            // Build completed between the caller's state_for() check and now; do not
            // sleep. The caller will re-check the cache on the next loop iteration.
            return (guard, false);
        };

        match condvar.wait_timeout(guard, timeout) {
            Ok((guard, result)) => (guard, result.timed_out()),
            Err(poisoned) => {
                warn!("search index manager wait was poisoned; recovering in-memory state");
                let (guard, result) = poisoned.into_inner();
                (guard, result.timed_out())
            }
        }
    }
}

#[derive(Debug)]
struct IndexManager {
    next_generation: u64,
    entries: HashMap<IndexKey, IndexEntry>,
    access_order: VecDeque<IndexKey>,
    cache_evictions: u64,
    /// Per-key build-in-progress signal map. When a build reserves a key, an
    /// `Arc<Condvar>` is inserted here so concurrent waiters can wait on the
    /// per-key condvar instead of a global notify_all storm. Removed and
    /// notified on build completion (success, failure, or cancellation).
    in_progress: HashMap<IndexKey, Arc<Condvar>>,
}

const DEFAULT_INDEX_CACHE_MAX_ENTRIES: usize = 8;
const DEFAULT_INDEX_CACHE_MAX_BYTES: u64 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexCacheLimits {
    max_entries: usize,
    max_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexCacheTelemetry {
    entries: usize,
    bytes: u64,
    evictions: u64,
    max_entries: usize,
    max_bytes: Option<u64>,
}

impl Default for IndexManager {
    fn default() -> Self {
        Self {
            next_generation: 1,
            entries: HashMap::new(),
            access_order: VecDeque::new(),
            cache_evictions: 0,
            in_progress: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexBuildReservation {
    generation: u64,
}

#[derive(Debug)]
enum SnapshotBuildDecision {
    Cached {
        snapshot: Arc<IndexSnapshot>,
        build_deduped: bool,
        cache_telemetry: IndexCacheTelemetry,
    },
    Build(IndexBuildReservation),
}

impl IndexManager {
    fn state_for(&self, key: &IndexKey) -> IndexEntryState {
        self.entries
            .get(key)
            .map_or(IndexEntryState::Cold, |entry| entry.state)
    }

    fn touch(&mut self, key: &IndexKey) {
        self.access_order.retain(|existing_key| existing_key != key);
        self.access_order.push_back(key.clone());
    }

    fn ready_entry_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.has_ready_snapshot())
            .count()
    }

    fn ready_cache_bytes(&self) -> u64 {
        self.entries
            .values()
            .map(Self::entry_cache_bytes)
            .fold(0_u64, u64::saturating_add)
    }

    fn entry_cache_bytes(entry: &IndexEntry) -> u64 {
        if !entry.has_ready_snapshot() {
            return 0;
        }

        entry
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.indexed_bytes)
    }

    fn cache_telemetry(&self, limits: IndexCacheLimits) -> IndexCacheTelemetry {
        IndexCacheTelemetry {
            entries: self.ready_entry_count(),
            bytes: self.ready_cache_bytes(),
            evictions: self.cache_evictions,
            max_entries: limits.max_entries,
            max_bytes: limits.max_bytes,
        }
    }

    fn reconcile_access_order(&mut self) {
        self.access_order.retain(|key| {
            self.entries
                .get(key)
                .is_some_and(IndexEntry::has_ready_snapshot)
        });

        let ordered: HashSet<IndexKey> = self.access_order.iter().cloned().collect();
        let missing: Vec<IndexKey> = self
            .entries
            .iter()
            .filter(|(key, entry)| entry.has_ready_snapshot() && !ordered.contains(*key))
            .map(|(key, _)| key.clone())
            .collect();
        self.access_order.extend(missing);
    }

    fn exceeds_cache_limits(&self, limits: IndexCacheLimits) -> bool {
        self.ready_entry_count() > limits.max_entries
            || limits
                .max_bytes
                .is_some_and(|max_bytes| self.ready_cache_bytes() > max_bytes)
    }

    fn evict_to_capacity(&mut self, limits: IndexCacheLimits) {
        self.reconcile_access_order();
        while self.exceeds_cache_limits(limits) {
            let Some(evicted_key) = self.access_order.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&evicted_key)
                .is_some_and(IndexEntry::has_ready_snapshot)
            {
                self.entries.remove(&evicted_key);
                self.cache_evictions = self.cache_evictions.saturating_add(1);
            }
        }
    }

    #[cfg(test)]
    fn cached_snapshot(&mut self, key: &IndexKey) -> Option<Arc<IndexSnapshot>> {
        self.cached_snapshot_with_limits(key, index_cache_limits())
    }

    fn cached_snapshot_with_limits(
        &mut self,
        key: &IndexKey,
        limits: IndexCacheLimits,
    ) -> Option<Arc<IndexSnapshot>> {
        let snapshot = self.entries.get(key)?.usable_snapshot()?;
        self.touch(key);
        self.evict_to_capacity(limits);
        Some(snapshot)
    }

    fn begin_build(&mut self, key: &IndexKey) -> IndexBuildReservation {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1).max(1);
        self.entries
            .entry(key.clone())
            .or_insert_with(IndexEntry::cold)
            .begin_build(generation);
        // Register the per-key condvar so concurrent waiters for this key can
        // be notified without disturbing waiters for unrelated keys.
        self.in_progress
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Condvar::new()));
        IndexBuildReservation { generation }
    }

    /// Remove and return the per-key condvar so the caller can notify waiters
    /// after releasing the manager mutex. Returns `None` if no build is in
    /// progress for this key.
    fn take_in_progress_condvar(&mut self, key: &IndexKey) -> Option<Arc<Condvar>> {
        self.in_progress.remove(key)
    }

    #[cfg(test)]
    fn publish_snapshot_if_absent(
        &mut self,
        key: IndexKey,
        snapshot: Arc<IndexSnapshot>,
    ) -> Arc<IndexSnapshot> {
        self.publish_snapshot_if_absent_with_limits(key, snapshot, index_cache_limits())
    }

    fn publish_snapshot_if_absent_with_limits(
        &mut self,
        key: IndexKey,
        snapshot: Arc<IndexSnapshot>,
        limits: IndexCacheLimits,
    ) -> Arc<IndexSnapshot> {
        if let Some(cached) = self.cached_snapshot_with_limits(&key, limits) {
            return cached;
        }

        let snapshot = {
            let entry = self
                .entries
                .entry(key.clone())
                .or_insert_with(IndexEntry::cold);
            entry.publish(snapshot);
            entry
                .snapshot
                .as_ref()
                .expect("published entry must have a snapshot")
                .clone()
        };
        self.touch(&key);
        self.evict_to_capacity(limits);

        snapshot
    }

    fn record_build_failure(
        &mut self,
        key: &IndexKey,
        reservation: IndexBuildReservation,
        err: &MemoryError,
    ) {
        if let Some(entry) = self.entries.get_mut(key)
            && entry.state == IndexEntryState::Building
            && entry.generation == Some(reservation.generation)
        {
            if err.is_per_request_failure() {
                entry.clear_failed_build();
            } else {
                entry.mark_unavailable(err);
            }
        }
    }

    fn begin_validation(&mut self, key: &IndexKey, snapshot: &Arc<IndexSnapshot>) -> bool {
        if let Some(entry) = self.entries.get_mut(key)
            && entry.current_snapshot_is(snapshot)
        {
            let full_validation_due = entry.full_validation_due();
            entry.mark_refreshing();
            return full_validation_due;
        }
        true
    }

    fn complete_validation(
        &mut self,
        key: &IndexKey,
        snapshot: &Arc<IndexSnapshot>,
        full_scope_ran: bool,
    ) -> IndexEntryState {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.mark_ready_after_validation(snapshot, full_scope_ran);
        }
        self.state_for(key)
    }

    fn record_validation_failure(
        &mut self,
        key: &IndexKey,
        snapshot: &Arc<IndexSnapshot>,
        err: &MemoryError,
    ) {
        if let Some(entry) = self.entries.get_mut(key)
            && entry.current_snapshot_is(snapshot)
        {
            entry.mark_unavailable(err);
            self.access_order.retain(|existing_key| existing_key != key);
        }
    }
}

#[derive(Debug)]
struct CachedSnapshot {
    key: IndexKey,
    snapshot: Arc<IndexSnapshot>,
    cache_status: &'static str,
    build_deduped: bool,
    cache_telemetry: IndexCacheTelemetry,
}

#[derive(Debug)]
struct WarmCacheConfig {
    enabled: bool,
    start_delay: Duration,
    key_delay: Duration,
    timeout_ms: u64,
    max_keys: usize,
    globs: Vec<String>,
    git_timeout: Duration,
}

impl WarmCacheConfig {
    fn from_env() -> Self {
        Self {
            enabled: env_bool("TOOLS_SEARCH_INDEX_WARM_ENABLED", true),
            start_delay: Duration::from_millis(
                env_u64(
                    "TOOLS_SEARCH_INDEX_WARM_START_DELAY_MS",
                    DEFAULT_WARM_CACHE_START_DELAY_MS,
                )
                .min(60_000),
            ),
            key_delay: Duration::from_millis(
                env_u64(
                    "TOOLS_SEARCH_INDEX_WARM_KEY_DELAY_MS",
                    DEFAULT_WARM_CACHE_KEY_DELAY_MS,
                )
                .min(60_000),
            ),
            timeout_ms: search_index_warm_timeout_ms(),
            max_keys: env_usize(
                "TOOLS_SEARCH_INDEX_WARM_MAX_KEYS",
                DEFAULT_WARM_CACHE_MAX_KEYS,
            )
            .clamp(1, 16),
            globs: warm_cache_globs_from_env(),
            git_timeout: Duration::from_millis(
                env_u64(
                    "TOOLS_SEARCH_INDEX_WARM_GIT_TIMEOUT_MS",
                    DEFAULT_WARM_CACHE_GIT_TIMEOUT_MS,
                )
                .clamp(100, 30_000),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WarmCacheKey {
    root: String,
    globs: Vec<String>,
    label: &'static str,
}

impl WarmCacheKey {
    fn repo_default(root: String) -> Self {
        Self {
            root,
            globs: Vec::new(),
            label: "repo-default",
        }
    }
}

#[derive(Debug)]
struct WarmCacheRunSummary {
    repo_root: PathBuf,
    keys_attempted: usize,
    keys_warmed: usize,
    keys_failed: usize,
    elapsed_ms: u128,
    warmed: Vec<WarmCacheSummary>,
}

#[derive(Debug)]
struct WarmCacheSummary {
    root: String,
    globs: Vec<String>,
    cache_status: &'static str,
    build_deduped: bool,
    generation: u64,
    indexed_files: usize,
    indexed_bytes: u64,
    elapsed_ms: u128,
}

#[derive(Clone, Debug)]
struct FuzzyVerifierSeed {
    pattern_start: usize,
    chars: Vec<char>,
}

#[derive(Clone, Debug)]
struct FuzzySeedPlan {
    partition_index: usize,
    seeds: Vec<Vec<u8>>,
    verifier_seeds: Vec<FuzzyVerifierSeed>,
}

#[derive(Clone, Debug)]
struct FuzzySeedSelection {
    partition_count: usize,
    partition_index: usize,
    candidate_seeds: Vec<Vec<u8>>,
    verifier_seeds: Vec<FuzzyVerifierSeed>,
    candidates: Vec<DocId>,
    duplicate_seed_count: usize,
    seed_candidate_counts: Vec<usize>,
    seed_byte_lengths: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct FuzzySeedPlanScore {
    candidate_count: usize,
    max_seed_candidate_count: usize,
    candidate_seed_count: usize,
    duplicate_seed_count: usize,
    shortest_seed_len: Reverse<usize>,
    longest_seed_len: Reverse<usize>,
    partition_index: usize,
}

#[derive(Clone, Debug)]
enum QueryPlan {
    Exact {
        literal: Vec<u8>,
        case: LiteralCase,
    },
    ShortExact {
        literal: Vec<u8>,
        case: LiteralCase,
    },
    WordExact {
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
        seed_plans: Vec<FuzzySeedPlan>,
    },
}

impl QueryPlan {
    fn kind(&self) -> &'static str {
        match self {
            Self::Exact { .. } | Self::ShortExact { .. } | Self::WordExact { .. } => "exact",
            Self::Regex { .. } => "regex",
            Self::Fuzzy { .. } => "fuzzy",
        }
    }

    fn requires_utf8_scope(&self) -> bool {
        matches!(self, Self::Regex { .. } | Self::Fuzzy { .. })
    }

    fn candidate_seed_count(&self) -> usize {
        match self {
            Self::Exact { literal, .. } | Self::WordExact { literal, .. } => {
                literal_trigrams(literal).len()
            }
            Self::ShortExact { .. } => 0,
            Self::Regex { candidates, .. } => candidate_expr_seed_count(candidates),
            Self::Fuzzy { seed_plans, .. } => seed_plans
                .first()
                .map(|plan| plan.seeds.len())
                .unwrap_or_default(),
        }
    }

    fn fuzzy_seed_count(&self) -> usize {
        match self {
            Self::Exact { .. }
            | Self::ShortExact { .. }
            | Self::WordExact { .. }
            | Self::Regex { .. } => 0,
            Self::Fuzzy { seed_plans, .. } => seed_plans
                .first()
                .map(|plan| plan.verifier_seeds.len())
                .unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CandidateExpr {
    Seed(Vec<u8>),
    And(Vec<CandidateExpr>),
    Or(Vec<CandidateExpr>),
}

#[derive(Default)]
struct CandidateEstimateCache {
    literal_estimates: HashMap<(Vec<u8>, LiteralCase), usize>,
    expr_estimates: HashMap<CandidateExpr, usize>,
}

#[derive(Default)]
struct FuzzySeedCandidateCache {
    candidates_by_seed: HashMap<Vec<u8>, Vec<DocId>>,
}

#[derive(Clone, Copy)]
struct LiteralPosting<'a> {
    trigram: [u8; 3],
    postings: &'a Postings,
}

#[derive(Clone, Debug)]
struct RegexDialectPlan {
    hir: Hir,
    decision: RegexDialectDecision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegexDialectDecision {
    memory_verifier: MemoryRegexVerifierBehavior,
    ugrep_behavior: UgrepRegexBehavior,
    fallback_reason: Option<RegexFallbackReason>,
}

impl RegexDialectDecision {
    fn eligible() -> Self {
        Self {
            memory_verifier: MemoryRegexVerifierBehavior::LineOrientedSafe,
            ugrep_behavior: UgrepRegexBehavior::SensitiveLineOriented,
            fallback_reason: None,
        }
    }

    fn fallback(
        memory_verifier: MemoryRegexVerifierBehavior,
        ugrep_behavior: UgrepRegexBehavior,
        fallback_reason: RegexFallbackReason,
    ) -> Self {
        Self {
            memory_verifier,
            ugrep_behavior,
            fallback_reason: Some(fallback_reason),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryRegexVerifierBehavior {
    LineOrientedSafe,
    RequiresUnsupportedCaseFolding,
    RequiresUnsupportedSmartCaseFolding,
    UnsupportedInlineConstruct,
    MayConsumeLineTerminator,
    ParserRejectedPattern,
    VerifierRejectedPattern,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UgrepRegexBehavior {
    SensitiveLineOriented,
    CaseInsensitiveLineOriented,
    SmartCaseInsensitiveLineOriented,
    DelegatedBackendDialect,
    DelegatedLineBreakDialect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegexFallbackReason {
    CaseInsensitive,
    SmartCaseInsensitive,
    Multiline,
    Backend,
}

impl RegexFallbackReason {
    fn error_type(self) -> &'static str {
        match self {
            Self::CaseInsensitive | Self::SmartCaseInsensitive => "unsupported_search_option",
            Self::Multiline | Self::Backend => "unsupported_regex_dialect",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::CaseInsensitive => "unsupported_regex_case_insensitive",
            Self::SmartCaseInsensitive => "unsupported_regex_smart_case_insensitive",
            Self::Multiline => "unsupported_multiline_regex",
            Self::Backend => "unsupported_regex_backend",
        }
    }
}

#[derive(Clone, Debug)]
struct RegexDialectFallback {
    decision: RegexDialectDecision,
    message: String,
}

impl RegexDialectFallback {
    fn new(
        memory_verifier: MemoryRegexVerifierBehavior,
        ugrep_behavior: UgrepRegexBehavior,
        fallback_reason: RegexFallbackReason,
        message: impl Into<String>,
    ) -> Self {
        Self {
            decision: RegexDialectDecision::fallback(
                memory_verifier,
                ugrep_behavior,
                fallback_reason,
            ),
            message: message.into(),
        }
    }

    fn into_memory_error(self) -> MemoryError {
        let fallback_reason = self
            .decision
            .fallback_reason
            .expect("regex dialect fallback must carry a reason");
        MemoryError::new(
            fallback_reason.error_type(),
            fallback_reason.as_str(),
            self.message,
        )
    }
}

#[derive(Clone, Debug)]
enum RegexDialectClassificationError {
    Fallback(RegexDialectFallback),
    Timeout(MemoryError),
}

impl RegexDialectClassificationError {
    fn into_memory_error(self) -> MemoryError {
        match self {
            Self::Fallback(fallback) => fallback.into_memory_error(),
            Self::Timeout(err) => err,
        }
    }
}

impl From<RegexDialectFallback> for RegexDialectClassificationError {
    fn from(fallback: RegexDialectFallback) -> Self {
        Self::Fallback(fallback)
    }
}

impl From<MemoryError> for RegexDialectClassificationError {
    fn from(err: MemoryError) -> Self {
        Self::Timeout(err)
    }
}

impl IndexKey {
    fn from_selector(selector: &FileSelector) -> Self {
        Self {
            root: selector.root_arg().to_string(),
            hidden: selector.include_hidden(),
            follow: selector.follow_links(),
            no_ignore: selector.no_ignore(),
            globs: selector.glob_key().to_vec(),
        }
    }
}

static INDEX_MANAGER: OnceLock<SharedIndexManager> = OnceLock::new();
static WARM_CACHE_STARTED: OnceLock<()> = OnceLock::new();

#[cfg(unix)]
type MetadataChangeMarker = UnixMetadataChangeMarker;

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UnixMetadataChangeMarker {
    dev: u64,
    ino: u64,
    mode: u32,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(windows)]
type MetadataChangeMarker = WindowsMetadataChangeMarker;

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowsMetadataChangeMarker {
    creation_time: u64,
    last_write_time: u64,
    file_attributes: u32,
}

#[cfg(not(any(unix, windows)))]
type MetadataChangeMarker = ();

fn index_manager() -> &'static SharedIndexManager {
    INDEX_MANAGER.get_or_init(SharedIndexManager::default)
}

fn lock_index_manager() -> MutexGuard<'static, IndexManager> {
    index_manager().lock()
}

fn acquire_or_reserve_snapshot_build(
    key: &IndexKey,
    deadline: Instant,
) -> Result<SnapshotBuildDecision, MemoryError> {
    let shared = index_manager();
    let mut manager = shared.lock();
    let mut build_deduped = false;

    loop {
        let cache_limits = index_cache_limits();
        if let Some(snapshot) = manager.cached_snapshot_with_limits(key, cache_limits) {
            let cache_telemetry = manager.cache_telemetry(cache_limits);
            return Ok(SnapshotBuildDecision::Cached {
                snapshot,
                build_deduped,
                cache_telemetry,
            });
        }

        if manager.state_for(key) == IndexEntryState::Building {
            #[cfg(test)]
            run_index_build_wait_test_hook(key);

            let now = Instant::now();
            if now >= deadline {
                return Err(MemoryError::timeout());
            }
            let (next_manager, timed_out) =
                shared.wait_for_build(manager, key, deadline.saturating_duration_since(now));
            manager = next_manager;
            build_deduped = true;
            if timed_out && manager.state_for(key) == IndexEntryState::Building {
                return Err(MemoryError::timeout());
            }
            continue;
        }

        if build_deduped
            && manager.state_for(key) == IndexEntryState::Unavailable
            && let Some(err) = manager
                .entries
                .get(key)
                .and_then(|entry| entry.last_error.clone())
        {
            return Err(err);
        }

        let reservation = manager.begin_build(key);
        return Ok(SnapshotBuildDecision::Build(reservation));
    }
}

fn publish_snapshot_if_absent(
    key: IndexKey,
    snapshot: Arc<IndexSnapshot>,
) -> (Arc<IndexSnapshot>, IndexCacheTelemetry) {
    let shared = index_manager();
    let (snapshot, cache_telemetry, condvar) = {
        let mut manager = shared.lock();
        let cache_limits = index_cache_limits();
        let snapshot =
            manager.publish_snapshot_if_absent_with_limits(key.clone(), snapshot, cache_limits);
        let cache_telemetry = manager.cache_telemetry(cache_limits);
        let condvar = manager.take_in_progress_condvar(&key);
        (snapshot, cache_telemetry, condvar)
    };
    if let Some(condvar) = condvar {
        condvar.notify_all();
    }
    (snapshot, cache_telemetry)
}

fn record_snapshot_build_failure(
    key: &IndexKey,
    reservation: IndexBuildReservation,
    err: &MemoryError,
) {
    let shared = index_manager();
    let condvar = {
        let mut manager = shared.lock();
        manager.record_build_failure(key, reservation, err);
        manager.take_in_progress_condvar(key)
    };
    if let Some(condvar) = condvar {
        condvar.notify_all();
    }
}

#[cfg(test)]
type IndexBuildTestHook = dyn Fn(&FileSelector, u64) + Send + Sync + 'static;

#[cfg(test)]
type IndexBuildWaitTestHook = dyn Fn(&IndexKey) + Send + Sync + 'static;

#[cfg(test)]
static INDEX_BUILD_TEST_HOOK: OnceLock<Mutex<Option<Arc<IndexBuildTestHook>>>> = OnceLock::new();

#[cfg(test)]
static INDEX_BUILD_WAIT_TEST_HOOK: OnceLock<Mutex<Option<Arc<IndexBuildWaitTestHook>>>> =
    OnceLock::new();

#[cfg(test)]
fn index_build_test_hook() -> &'static Mutex<Option<Arc<IndexBuildTestHook>>> {
    INDEX_BUILD_TEST_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn index_build_wait_test_hook() -> &'static Mutex<Option<Arc<IndexBuildWaitTestHook>>> {
    INDEX_BUILD_WAIT_TEST_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn replace_index_build_test_hook(
    hook: Option<Arc<IndexBuildTestHook>>,
) -> Option<Arc<IndexBuildTestHook>> {
    let mut guard = index_build_test_hook()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::replace(&mut *guard, hook)
}

#[cfg(test)]
fn replace_index_build_wait_test_hook(
    hook: Option<Arc<IndexBuildWaitTestHook>>,
) -> Option<Arc<IndexBuildWaitTestHook>> {
    let mut guard = index_build_wait_test_hook()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::replace(&mut *guard, hook)
}

#[cfg(test)]
fn run_index_build_test_hook(selector: &FileSelector, generation: u64) {
    let hook = index_build_test_hook()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(hook) = hook {
        hook(selector, generation);
    }
}

#[cfg(test)]
fn run_index_build_wait_test_hook(key: &IndexKey) {
    let hook = index_build_wait_test_hook()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(hook) = hook {
        hook(key);
    }
}

pub(super) fn start_search_cache_warmer() {
    let config = WarmCacheConfig::from_env();
    if !config.enabled {
        debug!("search index warm-cache startup is disabled");
        return;
    }
    if WARM_CACHE_STARTED.set(()).is_err() {
        debug!("search index warm-cache startup was already scheduled");
        return;
    }

    match std::thread::Builder::new()
        .name("tools-search-index-warm".to_string())
        .spawn(move || {
            if !config.start_delay.is_zero() {
                std::thread::sleep(config.start_delay);
            }
            debug!(
                start_delay_ms = config.start_delay.as_millis(),
                key_delay_ms = config.key_delay.as_millis(),
                max_keys = config.max_keys,
                globs = ?config.globs,
                "starting search index warm-cache thread"
            );
            match warm_cache_for_current_git_repo_blocking(config) {
                Ok(Some(summary)) => {
                    info!(
                        repo_root = %summary.repo_root.display(),
                        keys_attempted = summary.keys_attempted,
                        keys_warmed = summary.keys_warmed,
                        keys_failed = summary.keys_failed,
                        elapsed_ms = summary.elapsed_ms,
                        "warmed search index cache"
                    );
                    for key in summary.warmed {
                        debug!(
                            root = %key.root,
                            globs = ?key.globs,
                            cache_status = key.cache_status,
                            build_deduped = key.build_deduped,
                            generation = key.generation,
                            indexed_files = key.indexed_files,
                            indexed_bytes = key.indexed_bytes,
                            elapsed_ms = key.elapsed_ms,
                            "warmed search index cache key"
                        );
                    }
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
    req: &NormalizedSearchRequest,
) -> Result<ToolCallOutcome, MemoryError> {
    let limits = Limits::from_env();
    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms());
    let plan = eligible_query_plan_with_limits(req, &limits, deadline)?;
    validate_plan_limits(&plan, &limits)?;
    let index_lookup_start = Instant::now();
    let cached = get_or_build_snapshot(req, &limits, deadline, plan.requires_utf8_scope())?;
    let index_lookup_ms = index_lookup_start.elapsed().as_millis() as u64;
    let snapshot = cached.snapshot;

    check_deadline(deadline)?;
    let phase_one_start = Instant::now();
    let mut estimate_cache = CandidateEstimateCache::default();
    let mut fuzzy_candidate_cache = FuzzySeedCandidateCache::default();
    let fuzzy_seed_selection = select_fuzzy_seed_plan_for_query_with_cache(
        &snapshot,
        &plan,
        &limits,
        deadline,
        &mut fuzzy_candidate_cache,
    )?;
    let candidate_estimate = fuzzy_seed_selection.as_ref().map_or_else(
        || {
            plan_candidate_estimate_with_cache(
                &snapshot,
                &plan,
                &limits,
                deadline,
                &mut estimate_cache,
            )
        },
        |selection| Ok(selection.candidates.len()),
    )?;
    let candidates = if candidate_estimate == 0 {
        Vec::new()
    } else if let Some(selection) = &fuzzy_seed_selection {
        selection.candidates.clone()
    } else {
        candidates_for_plan_with_cache(
            &snapshot,
            &plan,
            &limits,
            deadline,
            &mut estimate_cache,
            &mut fuzzy_candidate_cache,
        )?
    };
    let phase_one_ms = phase_one_start.elapsed().as_millis() as u64;

    check_deadline(deadline)?;
    let phase_two_start = Instant::now();
    let (events, truncated, verification_stats, result_doc_ids) = verify_and_render(
        &snapshot,
        &candidates,
        &plan,
        fuzzy_seed_selection.as_ref(),
        req,
        &limits,
        deadline,
    )?;
    let phase_two_ms = phase_two_start.elapsed().as_millis() as u64;

    let freshness_validation = SnapshotValidation::targeted(req, result_doc_ids);
    let freshness_start = Instant::now();
    let freshness_result =
        validate_cached_snapshot_fresh(&cached.key, &snapshot, freshness_validation, deadline)?;
    let freshness_check_ms = freshness_start.elapsed().as_millis() as u64;

    let rendered_events = render_search_events(&events);
    let text_view = render_search_text_with_deadline(&rendered_events, deadline)?;
    let exit_code = if events.iter().any(|event| event.is_match) {
        0
    } else {
        1
    };
    let mut payload = build_search_payload_from_rendered(
        req,
        SearchPayloadMeta::new(
            req.root(),
            text_view,
            false,
            json!(exit_code),
            truncated,
            false,
        ),
        &rendered_events,
    );

    if let Some(obj) = payload.as_object_mut() {
        obj.insert("backend".to_string(), json!("memory"));
        obj.insert("plan_kind".to_string(), json!(plan.kind()));
        obj.insert("memory_eligibility".to_string(), json!("eligible"));
        obj.insert("index_cache".to_string(), json!(cached.cache_status));
        obj.insert(
            "index_build_deduped".to_string(),
            json!(cached.build_deduped),
        );
        obj.insert(
            "index_state".to_string(),
            json!(freshness_result.index_state.as_str()),
        );
        obj.insert("index_generation".to_string(), json!(snapshot.generation));
        obj.insert("indexed_files".to_string(), json!(snapshot.documents.len()));
        obj.insert("indexed_bytes".to_string(), json!(snapshot.indexed_bytes));
        obj.insert(
            "cache_entries".to_string(),
            json!(cached.cache_telemetry.entries),
        );
        obj.insert(
            "cache_bytes".to_string(),
            json!(cached.cache_telemetry.bytes),
        );
        obj.insert(
            "cache_evictions".to_string(),
            json!(cached.cache_telemetry.evictions),
        );
        obj.insert(
            "cache_max_entries".to_string(),
            json!(cached.cache_telemetry.max_entries),
        );
        obj.insert(
            "cache_max_bytes".to_string(),
            cached
                .cache_telemetry
                .max_bytes
                .map_or(Value::Null, |max_bytes| json!(max_bytes)),
        );
        obj.insert("index_lookup_ms".to_string(), json!(index_lookup_ms));
        obj.insert("candidate_estimate".to_string(), json!(candidate_estimate));
        obj.insert("candidate_count".to_string(), json!(candidates.len()));
        obj.insert(
            "candidate_seed_count".to_string(),
            json!(fuzzy_seed_selection.as_ref().map_or_else(
                || plan.candidate_seed_count(),
                |selection| selection.candidate_seeds.len()
            )),
        );
        obj.insert("candidate_limit".to_string(), json!(limits.max_candidates));
        obj.insert(
            "fuzzy_seed_count".to_string(),
            json!(fuzzy_seed_selection.as_ref().map_or_else(
                || plan.fuzzy_seed_count(),
                |selection| selection.verifier_seeds.len()
            )),
        );
        if let Some(selection) = &fuzzy_seed_selection {
            obj.insert(
                "fuzzy_seed_partition_count".to_string(),
                json!(selection.partition_count),
            );
            obj.insert(
                "fuzzy_seed_selected_partition".to_string(),
                json!(selection.partition_index),
            );
            obj.insert(
                "fuzzy_candidate_seed_count".to_string(),
                json!(selection.candidate_seeds.len()),
            );
            obj.insert(
                "fuzzy_duplicate_seed_count".to_string(),
                json!(selection.duplicate_seed_count),
            );
            obj.insert(
                "fuzzy_seed_candidate_counts".to_string(),
                json!(selection.seed_candidate_counts),
            );
            obj.insert(
                "fuzzy_seed_byte_lengths".to_string(),
                json!(selection.seed_byte_lengths),
            );
        }
        obj.insert(
            "fuzzy_verified_lines".to_string(),
            json!(verification_stats.fuzzy_verified_lines),
        );
        obj.insert(
            "verified_line_count".to_string(),
            json!(verification_stats.verified_lines),
        );
        obj.insert("max_results_limit".to_string(), json!(req.max_results()));
        obj.insert("max_results_reached".to_string(), json!(truncated));
        obj.insert("phase_one_ms".to_string(), json!(phase_one_ms));
        obj.insert("phase_two_ms".to_string(), json!(phase_two_ms));
        obj.insert(
            "freshness_check".to_string(),
            json!(freshness_result.status),
        );
        obj.insert(
            "freshness_scope".to_string(),
            json!(freshness_result.scope.as_str()),
        );
        obj.insert(
            "freshness_index_state".to_string(),
            json!(freshness_result.index_state.as_str()),
        );
        obj.insert("freshness_state".to_string(), json!(freshness_result.state));
        obj.insert(
            "freshness_result_files_checked".to_string(),
            json!(freshness_result.stats.result_files_checked),
        );
        obj.insert(
            "freshness_indexed_files_checked".to_string(),
            json!(freshness_result.stats.indexed_files_checked),
        );
        obj.insert(
            "freshness_directories_checked".to_string(),
            json!(freshness_result.stats.directories_checked),
        );
        obj.insert(
            "freshness_full_scope_scans".to_string(),
            json!(freshness_result.stats.full_scope_scans),
        );
        obj.insert(
            "freshness_full_scan_reason".to_string(),
            freshness_result
                .full_scan_reason
                .map_or(Value::Null, |reason| json!(reason)),
        );
        obj.insert("freshness_check_ms".to_string(), json!(freshness_check_ms));
    }

    Ok(ToolCallOutcome::ok(payload))
}

fn get_or_build_snapshot(
    req: &NormalizedSearchRequest,
    limits: &Limits,
    deadline: Instant,
    require_utf8_scope: bool,
) -> Result<CachedSnapshot, MemoryError> {
    let selector = FileSelector::for_memory(req).map_err(MemoryError::from)?;
    let key = IndexKey::from_selector(&selector);
    get_or_build_snapshot_with_selector(selector, key, limits, deadline, require_utf8_scope)
}

fn get_or_build_snapshot_with_selector(
    selector: FileSelector,
    key: IndexKey,
    limits: &Limits,
    deadline: Instant,
    require_utf8_scope: bool,
) -> Result<CachedSnapshot, MemoryError> {
    match acquire_or_reserve_snapshot_build(&key, deadline)? {
        SnapshotBuildDecision::Cached {
            snapshot,
            build_deduped,
            cache_telemetry,
        } => {
            ensure_snapshot_supports_query(&snapshot, require_utf8_scope)?;
            Ok(CachedSnapshot {
                key,
                snapshot,
                cache_status: "hit",
                build_deduped,
                cache_telemetry,
            })
        }
        SnapshotBuildDecision::Build(reservation) => {
            let snapshot = match build_index_with_selector(
                &selector,
                limits,
                deadline,
                require_utf8_scope,
                reservation.generation,
            ) {
                Ok(snapshot) => Arc::new(snapshot),
                Err(err) => {
                    record_snapshot_build_failure(&key, reservation, &err);
                    return Err(err);
                }
            };
            let (snapshot, cache_telemetry) = publish_snapshot_if_absent(key.clone(), snapshot);
            ensure_snapshot_supports_query(&snapshot, require_utf8_scope)?;
            Ok(CachedSnapshot {
                key,
                snapshot,
                cache_status: "miss",
                build_deduped: false,
                cache_telemetry,
            })
        }
    }
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

fn warm_cache_for_current_git_repo_blocking(
    config: WarmCacheConfig,
) -> Result<Option<WarmCacheRunSummary>, String> {
    let cwd = std::env::current_dir().map_err(|err| format!("failed to read cwd: {err}"))?;
    let Some(repo_root) = git_worktree_root_from_with_timeout(&cwd, config.git_timeout) else {
        return Ok(None);
    };
    let keys = likely_warm_cache_keys(&cwd, &repo_root, &config);
    Ok(Some(warm_cache_for_keys(repo_root, keys, &config)))
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

fn likely_warm_cache_keys(
    cwd: &Path,
    repo_root: &Path,
    config: &WarmCacheConfig,
) -> Vec<WarmCacheKey> {
    let repo_root_arg = warm_cache_root_argument(cwd, repo_root);
    let cwd_differs = !paths_refer_to_same_location(cwd, repo_root);
    let mut keys = Vec::new();

    // Realistic first call: no glob, repo-default scope.
    push_warm_cache_key(&mut keys, WarmCacheKey::repo_default(repo_root_arg.clone()));

    // Same realistic first call but rooted at the cwd (skipped when cwd matches
    // the repo root to avoid paying for the same scope twice).
    if cwd_differs {
        push_warm_cache_key(
            &mut keys,
            WarmCacheKey {
                root: ".".to_string(),
                globs: Vec::new(),
                label: "cwd-default",
            },
        );
    }

    // Common single-glob queries against the repo root.
    for glob in &config.globs {
        push_warm_cache_key(
            &mut keys,
            WarmCacheKey {
                root: repo_root_arg.clone(),
                globs: vec![glob.clone()],
                label: "repo-glob",
            },
        );
    }

    // Same single-glob queries but rooted at the cwd when it differs, so
    // searches initiated from a subdirectory also benefit from warm caches.
    if cwd_differs {
        for glob in &config.globs {
            push_warm_cache_key(
                &mut keys,
                WarmCacheKey {
                    root: ".".to_string(),
                    globs: vec![glob.clone()],
                    label: "cwd-glob",
                },
            );
        }
    }

    keys.truncate(config.max_keys);
    keys
}

fn push_warm_cache_key(keys: &mut Vec<WarmCacheKey>, key: WarmCacheKey) {
    if !keys
        .iter()
        .any(|existing| existing.root == key.root && existing.globs == key.globs)
    {
        keys.push(key);
    }
}

fn warm_cache_for_keys(
    repo_root: PathBuf,
    keys: Vec<WarmCacheKey>,
    config: &WarmCacheConfig,
) -> WarmCacheRunSummary {
    let started = Instant::now();
    let keys_attempted = keys.len();
    let mut warmed = Vec::new();
    let mut keys_failed = 0;
    let cancel_token = current_cancellation_token();

    for (index, key) in keys.iter().enumerate() {
        if let Some(token) = cancel_token.as_ref()
            && token.is_cancelled()
        {
            debug!("search index warm-cache loop observed cancellation; stopping early");
            break;
        }
        if index > 0 && !config.key_delay.is_zero() {
            std::thread::sleep(config.key_delay);
        }

        match warm_cache_for_key(key, config.timeout_ms) {
            Ok(summary) => warmed.push(summary),
            Err(err) => {
                keys_failed += 1;
                debug!(
                    root = %key.root,
                    globs = ?key.globs,
                    label = key.label,
                    error = %err.message,
                    error_type = err.error_type,
                    fallback_reason = err.fallback_reason,
                    "search index warm-cache key did not populate"
                );
            }
        }
    }

    WarmCacheRunSummary {
        repo_root,
        keys_attempted,
        keys_warmed: warmed.len(),
        keys_failed,
        elapsed_ms: started.elapsed().as_millis(),
        warmed,
    }
}

#[cfg(test)]
fn warm_cache_for_root(root: String) -> Result<WarmCacheSummary, MemoryError> {
    warm_cache_for_key(
        &WarmCacheKey::repo_default(root),
        search_index_warm_timeout_ms(),
    )
}

fn warm_cache_for_key(
    key: &WarmCacheKey,
    timeout_ms: u64,
) -> Result<WarmCacheSummary, MemoryError> {
    let started = Instant::now();
    let req = SearchRequest {
        pattern: WARM_CACHE_PATTERN.to_string(),
        path: Some(key.root.clone()),
        case: Some("sensitive".to_string()),
        fixed_strings: Some(true),
        word_regexp: Some(false),
        glob: (!key.globs.is_empty()).then(|| key.globs.clone()),
        hidden: Some(false),
        follow: Some(false),
        no_ignore: Some(false),
        context: None,
        max_results: None,
        timeout_ms: Some(timeout_ms),
        fuzzy: None,
    }
    .normalize();
    let limits = Limits::from_env();
    let deadline = Instant::now() + Duration::from_millis(req.timeout_ms());
    let selector = FileSelector::for_memory(&req)?;
    let summary_root = key.root.clone();
    let summary_globs = key.globs.clone();
    let index_key = IndexKey::from_selector(&selector);
    let cached =
        get_or_build_snapshot_with_selector(selector, index_key, &limits, deadline, false)?;

    Ok(WarmCacheSummary {
        root: summary_root,
        globs: summary_globs,
        cache_status: cached.cache_status,
        build_deduped: cached.build_deduped,
        generation: cached.snapshot.generation,
        indexed_files: cached.snapshot.documents.len(),
        indexed_bytes: cached.snapshot.indexed_bytes,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[cfg(test)]
fn git_worktree_root_from(cwd: &Path) -> Option<PathBuf> {
    git_worktree_root_from_with_timeout(cwd, search_index_warm_git_timeout())
}

fn git_worktree_root_from_with_timeout(cwd: &Path, timeout: Duration) -> Option<PathBuf> {
    let start = fs::canonicalize(cwd).ok()?;
    git_worktree_root_from_markers(&start)
        .or_else(|| git_worktree_root_from_command(&start, timeout))
}

fn git_worktree_root_from_markers(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| has_git_worktree_marker(dir))
        .map(Path::to_path_buf)
}

fn git_worktree_root_from_command(cwd: &Path, timeout: Duration) -> Option<PathBuf> {
    let mut child = Command::new(git_bin())
        .arg("-C")
        .arg(cwd)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();

    loop {
        if let Some(status) = child.try_wait().ok()? {
            if !status.success() {
                return None;
            }

            let mut output = Vec::new();
            child.stdout.take()?.read_to_end(&mut output).ok()?;
            let root = parse_git_toplevel_output(&output)?;
            return fs::canonicalize(&root).ok().or(Some(root));
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

fn parse_git_toplevel_output(output: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(output).ok()?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
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
fn search_index_warm_git_timeout() -> Duration {
    Duration::from_millis(
        env_u64(
            "TOOLS_SEARCH_INDEX_WARM_GIT_TIMEOUT_MS",
            DEFAULT_WARM_CACHE_GIT_TIMEOUT_MS,
        )
        .clamp(100, 30_000),
    )
}

#[cfg(test)]
fn eligible_query_plan(req: &SearchRequest) -> Result<QueryPlan, MemoryError> {
    let limits = Limits::from_env();
    let req = req.normalize();
    eligible_query_plan_with_limits(&req, &limits, Instant::now() + Duration::from_secs(60))
}

fn eligible_query_plan_with_limits(
    req: &NormalizedSearchRequest,
    limits: &Limits,
    deadline: Instant,
) -> Result<QueryPlan, MemoryError> {
    if let Some(distance) = req.raw_fuzzy() {
        return eligible_fuzzy_plan(req, distance, limits, deadline);
    }

    let fixed_strings = req.fixed_strings();
    if req.word_regexp() && !fixed_strings {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_word_regexp",
            "memory search only supports word_regexp for fixed_strings=true ASCII literals",
        ));
    }
    if req.word_regexp() && !req.pattern().is_ascii() {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_word_regexp",
            "memory search only supports word_regexp for ASCII fixed strings",
        ));
    }
    if req.follow() {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_follow",
            "memory search does not support following symlinks",
        ));
    }
    if !fixed_strings && !is_plain_regex_literal(req.pattern()) {
        return eligible_regex_plan(req, limits, deadline);
    }
    let case = literal_case_for_request(req, fixed_strings)?;

    let literal = req.pattern().as_bytes();
    if literal.len() < 3 {
        return eligible_short_literal_plan(req, literal, case, fixed_strings);
    }
    if literal.contains(&b'\n') || literal.contains(&b'\r') {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_multiline_literal",
            "memory search does not support multiline fixed strings",
        ));
    }
    if req.word_regexp() {
        if !is_supported_ascii_word_literal(literal) {
            return Err(MemoryError::new(
                "unsupported_search_option",
                "unsupported_word_regexp",
                "memory search only supports word_regexp for ASCII literals bounded by word bytes",
            ));
        }
        Ok(QueryPlan::WordExact {
            literal: literal.to_vec(),
            case,
        })
    } else {
        Ok(QueryPlan::Exact {
            literal: literal.to_vec(),
            case,
        })
    }
}

fn eligible_short_literal_plan(
    req: &NormalizedSearchRequest,
    literal: &[u8],
    case: LiteralCase,
    fixed_strings: bool,
) -> Result<QueryPlan, MemoryError> {
    if !fixed_strings || req.word_regexp() || literal.contains(&b'\n') || literal.contains(&b'\r') {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "query_without_required_trigram",
            "memory search requires a literal of at least three bytes",
        ));
    }

    if case == LiteralCase::AsciiInsensitive && !literal.is_ascii() {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_non_ascii_short_literal_case",
            "memory search only supports ASCII case folding for short fixed strings",
        ));
    }

    Ok(QueryPlan::ShortExact {
        literal: literal.to_vec(),
        case,
    })
}

fn eligible_regex_plan(
    req: &NormalizedSearchRequest,
    limits: &Limits,
    deadline: Instant,
) -> Result<QueryPlan, MemoryError> {
    check_deadline(deadline)?;
    let dialect = classify_regex_dialect_for_planning(req, deadline)?;
    debug_assert_eq!(dialect.decision, RegexDialectDecision::eligible());
    let candidates = required_candidate_expr(&dialect.hir, deadline)?.ok_or_else(|| {
        MemoryError::new(
            "unsupported_regex_dialect",
            "query_without_required_trigram",
            "memory regex search requires a proven literal substring of at least three bytes",
        )
    })?;
    check_deadline(deadline)?;

    let matcher = build_classified_regex_matcher(req.pattern(), limits)
        .map_err(RegexDialectFallback::into_memory_error)?;

    Ok(QueryPlan::Regex {
        matcher,
        candidates,
    })
}

fn classify_regex_dialect_for_planning(
    req: &NormalizedSearchRequest,
    deadline: Instant,
) -> Result<RegexDialectPlan, MemoryError> {
    classify_regex_dialect_for_planning_inner(req, deadline)
        .map_err(RegexDialectClassificationError::into_memory_error)
}

fn classify_regex_dialect_for_planning_inner(
    req: &NormalizedSearchRequest,
    deadline: Instant,
) -> Result<RegexDialectPlan, RegexDialectClassificationError> {
    classify_regex_case(req)?;
    classify_regex_surface_syntax(req.pattern())?;

    let hir = RegexParserBuilder::new()
        .utf8(true)
        .unicode(true)
        .build()
        .parse(req.pattern())
        .map_err(|err| {
            RegexDialectFallback::new(
                MemoryRegexVerifierBehavior::ParserRejectedPattern,
                UgrepRegexBehavior::DelegatedBackendDialect,
                RegexFallbackReason::Backend,
                format!("memory regex search could not parse the pattern: {err}"),
            )
        })?;

    check_deadline(deadline)?;
    if hir_can_match_lf(&hir) {
        return Err(RegexDialectFallback::new(
            MemoryRegexVerifierBehavior::MayConsumeLineTerminator,
            UgrepRegexBehavior::DelegatedLineBreakDialect,
            RegexFallbackReason::Multiline,
            "memory regex search does not support regex patterns that can match line breaks",
        )
        .into());
    }

    Ok(RegexDialectPlan {
        hir,
        decision: RegexDialectDecision::eligible(),
    })
}

fn classify_regex_case(req: &NormalizedSearchRequest) -> Result<(), RegexDialectFallback> {
    match req.case_mode() {
        SearchCaseMode::Sensitive => Ok(()),
        SearchCaseMode::Insensitive => Err(RegexDialectFallback::new(
            MemoryRegexVerifierBehavior::RequiresUnsupportedCaseFolding,
            UgrepRegexBehavior::CaseInsensitiveLineOriented,
            RegexFallbackReason::CaseInsensitive,
            "memory regex search does not support case-insensitive regex matching",
        )),
        SearchCaseMode::Smart => {
            if contains_uppercase_letter(req.pattern()) {
                Ok(())
            } else {
                Err(RegexDialectFallback::new(
                    MemoryRegexVerifierBehavior::RequiresUnsupportedSmartCaseFolding,
                    UgrepRegexBehavior::SmartCaseInsensitiveLineOriented,
                    RegexFallbackReason::SmartCaseInsensitive,
                    "memory regex search falls back for smart-case regexes that would be case-insensitive",
                ))
            }
        }
    }
}

fn classify_regex_surface_syntax(pattern: &str) -> Result<(), RegexDialectFallback> {
    if has_inline_regex_construct(pattern) {
        return Err(RegexDialectFallback::new(
            MemoryRegexVerifierBehavior::UnsupportedInlineConstruct,
            UgrepRegexBehavior::DelegatedBackendDialect,
            RegexFallbackReason::Backend,
            "memory regex search does not support inline flags, look-around, or special group syntax",
        ));
    }
    if pattern.contains('\n') || pattern.contains('\r') {
        return Err(RegexDialectFallback::new(
            MemoryRegexVerifierBehavior::MayConsumeLineTerminator,
            UgrepRegexBehavior::DelegatedLineBreakDialect,
            RegexFallbackReason::Multiline,
            "memory regex search does not support multiline regex patterns",
        ));
    }
    if has_line_break_escape(pattern) {
        return Err(RegexDialectFallback::new(
            MemoryRegexVerifierBehavior::MayConsumeLineTerminator,
            UgrepRegexBehavior::DelegatedLineBreakDialect,
            RegexFallbackReason::Multiline,
            "memory regex search does not support regex patterns that match line breaks",
        ));
    }
    Ok(())
}

fn build_classified_regex_matcher(
    pattern: &str,
    limits: &Limits,
) -> Result<Regex, RegexDialectFallback> {
    let mut builder = RegexBuilder::new(pattern);
    builder
        .unicode(true)
        .case_insensitive(false)
        .multi_line(false)
        .dot_matches_new_line(false)
        .size_limit(limits.regex_size_limit_bytes);
    builder.build().map_err(|err| {
        RegexDialectFallback::new(
            MemoryRegexVerifierBehavior::VerifierRejectedPattern,
            UgrepRegexBehavior::DelegatedBackendDialect,
            RegexFallbackReason::Backend,
            format!("memory regex verifier could not compile the pattern: {err}"),
        )
    })
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
    req: &NormalizedSearchRequest,
    fixed_strings: bool,
) -> Result<LiteralCase, MemoryError> {
    match req.case_mode() {
        SearchCaseMode::Sensitive => Ok(LiteralCase::Sensitive),
        SearchCaseMode::Insensitive => {
            if fixed_strings || req.pattern().is_ascii() {
                Ok(LiteralCase::AsciiInsensitive)
            } else {
                Err(MemoryError::new(
                    "unsupported_search_option",
                    "unsupported_unicode_regex_case_insensitive",
                    "memory regex literal search does not support Unicode case-insensitive matching",
                ))
            }
        }
        SearchCaseMode::Smart => {
            if contains_uppercase_letter(req.pattern()) {
                Ok(LiteralCase::Sensitive)
            } else if fixed_strings || req.pattern().is_ascii() {
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

fn eligible_fuzzy_plan(
    req: &NormalizedSearchRequest,
    distance: u8,
    limits: &Limits,
    deadline: Instant,
) -> Result<QueryPlan, MemoryError> {
    check_deadline(deadline)?;
    if !req.fixed_strings() {
        return Err(MemoryError::new(
            "unsupported_regex_dialect",
            "unsupported_regex_fuzzy",
            "memory search only supports fuzzy matching for fixed_strings=true",
        ));
    }
    if req.word_regexp() {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_word_fuzzy",
            "memory search does not support fuzzy word_regexp",
        ));
    }
    if req.follow() {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_fuzzy_follow",
            "memory search does not support following symlinks for fuzzy matching",
        ));
    }
    match req.case_mode() {
        SearchCaseMode::Sensitive => {}
        SearchCaseMode::Smart | SearchCaseMode::Insensitive => {
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
    if req.pattern().contains('\n') || req.pattern().contains('\r') {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "unsupported_multiline_fuzzy",
            "memory search does not support multiline fuzzy fixed strings",
        ));
    }
    if char_count_exceeds(req.pattern(), limits.max_fuzzy_pattern_chars, deadline)? {
        return Err(MemoryError::new(
            "resource_limit_exceeded",
            "max_fuzzy_pattern_chars_exceeded",
            "fuzzy pattern exceeds memory search verifier limit",
        ));
    }

    let seed_count = usize::from(distance) + 1;
    if req.pattern().len() < seed_count * 3 {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "fuzzy_pattern_too_short",
            "fuzzy memory search requires at least three bytes per seed",
        ));
    }
    check_deadline(deadline)?;
    let partitions = fuzzy_seed_partitions_with_deadline(req.pattern(), distance, deadline)?;
    if partitions.is_empty() {
        return Err(MemoryError::new(
            "unsupported_search_option",
            "fuzzy_pattern_unseedable",
            "fuzzy fixed-string pattern cannot be partitioned into required seed segments",
        ));
    }
    let pattern_chars: Vec<char> = req.pattern().chars().collect();
    let mut seed_plans = Vec::with_capacity(partitions.len());
    for (partition_index, partition) in partitions.iter().enumerate() {
        check_deadline(deadline)?;
        seed_plans.push(FuzzySeedPlan {
            partition_index,
            seeds: fuzzy_candidate_seeds_from_partition(req.pattern(), partition),
            verifier_seeds: fuzzy_verifier_seeds_from_partition(&pattern_chars, partition),
        });
    }
    check_deadline(deadline)?;

    Ok(QueryPlan::Fuzzy {
        pattern_chars,
        distance: usize::from(distance),
        seed_plans,
    })
}

#[cfg(test)]
fn build_index(
    req: &NormalizedSearchRequest,
    limits: &Limits,
    deadline: Instant,
    require_utf8_scope: bool,
    generation: u64,
) -> Result<IndexSnapshot, MemoryError> {
    let selector = FileSelector::for_memory(req).map_err(MemoryError::from)?;
    build_index_with_selector(&selector, limits, deadline, require_utf8_scope, generation)
}

fn build_index_with_selector(
    selector: &FileSelector,
    limits: &Limits,
    deadline: Instant,
    require_utf8_scope: bool,
    generation: u64,
) -> Result<IndexSnapshot, MemoryError> {
    #[cfg(test)]
    run_index_build_test_hook(selector, generation);

    let cancel_token = current_cancellation_token();
    let cancel = cancel_token.as_ref();

    let mut documents = Vec::new();
    let mut indexed_bytes = 0_u64;
    let mut all_content_utf8 = true;

    let discovered_scope = selector
        .discover_memory_scope(Some(deadline))
        .map_err(MemoryError::from)?;
    let ignore_fingerprint = discovered_scope.ignore_fingerprint;
    let scope_fingerprint =
        ScopeFingerprint::from_directories(discovered_scope.directories, deadline)?;

    for path in discovered_scope.files {
        check_cancellation(cancel)?;
        check_deadline(deadline)?;
        let metadata = fs::metadata(&path).map_err(|err| {
            MemoryError::new(
                "search_index_incomplete",
                "metadata_error",
                format!("failed to read metadata for {}: {err}", path.display()),
            )
        })?;
        check_deadline(deadline)?;

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

        check_deadline(deadline)?;
        let content = fs::read(&path).map_err(|err| {
            MemoryError::new(
                "search_index_incomplete",
                "read_error",
                format!("failed to read {}: {err}", path.display()),
            )
        })?;
        check_deadline(deadline)?;
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

        check_deadline(deadline)?;
        let stamp = file_stamp_from_parts_with_deadline(&metadata, deadline)?;
        let rendered_path = selector.render_path(&path);
        let lines = line_ranges_with_deadline(&content, deadline)?;
        check_deadline(deadline)?;
        documents.push(Document {
            path,
            rendered_path,
            stamp,
            lines,
            content,
        });
    }

    check_deadline(deadline)?;
    documents.sort_by(|left, right| left.rendered_path.cmp(&right.rendered_path));

    let mut postings: HashMap<[u8; 3], Vec<DocId>> = HashMap::new();
    let mut ascii_folded_postings: HashMap<[u8; 3], Vec<DocId>> = HashMap::new();
    for (doc_index, doc) in documents.iter().enumerate() {
        check_cancellation(cancel)?;
        check_deadline(deadline)?;
        let doc_id = DocId::from_index(doc_index)?;
        index_document_trigrams_with_deadline(
            doc_id,
            &doc.content,
            &mut postings,
            &mut ascii_folded_postings,
            deadline,
        )?;
    }
    let postings = PostingsIndex::from_raw(postings, deadline)?;
    let ascii_folded_postings = PostingsIndex::from_raw(ascii_folded_postings, deadline)?;
    check_deadline(deadline)?;

    Ok(IndexSnapshot {
        generation,
        documents,
        scope_fingerprint,
        ignore_fingerprint,
        postings,
        ascii_folded_postings,
        indexed_bytes,
        all_content_utf8,
    })
}

fn discover_files(
    req: &NormalizedSearchRequest,
    deadline: Option<Instant>,
) -> Result<Vec<PathBuf>, MemoryError> {
    FileSelector::for_memory(req)
        .map_err(MemoryError::from)?
        .discover_memory_files(deadline)
        .map_err(MemoryError::from)
}

#[cfg(test)]
fn candidates_for_plan(
    snapshot: &IndexSnapshot,
    plan: &QueryPlan,
    limits: &Limits,
    deadline: Instant,
) -> Result<Vec<DocId>, MemoryError> {
    let mut estimate_cache = CandidateEstimateCache::default();
    let mut fuzzy_candidate_cache = FuzzySeedCandidateCache::default();
    candidates_for_plan_with_cache(
        snapshot,
        plan,
        limits,
        deadline,
        &mut estimate_cache,
        &mut fuzzy_candidate_cache,
    )
}

fn candidates_for_plan_with_cache(
    snapshot: &IndexSnapshot,
    plan: &QueryPlan,
    limits: &Limits,
    deadline: Instant,
    estimate_cache: &mut CandidateEstimateCache,
    fuzzy_candidate_cache: &mut FuzzySeedCandidateCache,
) -> Result<Vec<DocId>, MemoryError> {
    check_cancellation(current_cancellation_token().as_ref())?;
    check_deadline(deadline)?;
    match plan {
        QueryPlan::Exact { literal, case } => {
            candidates_for_literal(snapshot, literal, *case, limits.max_candidates, deadline)
        }
        QueryPlan::ShortExact { literal, case } => {
            candidates_for_short_literal_direct_scan(snapshot, literal, *case, limits, deadline)
        }
        QueryPlan::WordExact { literal, case } => {
            candidates_for_literal(snapshot, literal, *case, limits.max_candidates, deadline)
        }
        QueryPlan::Regex { candidates, .. } => candidates_for_candidate_expr_with_cache(
            snapshot,
            candidates,
            limits.max_candidates,
            deadline,
            estimate_cache,
        ),
        QueryPlan::Fuzzy { .. } => {
            let selection = select_fuzzy_seed_plan_with_cache(
                snapshot,
                plan,
                limits,
                deadline,
                fuzzy_candidate_cache,
            )?;
            Ok(selection.candidates)
        }
    }
}

#[cfg(test)]
fn candidates_for_candidate_expr(
    snapshot: &IndexSnapshot,
    expr: &CandidateExpr,
    max_candidates: usize,
    deadline: Instant,
) -> Result<Vec<DocId>, MemoryError> {
    let mut estimate_cache = CandidateEstimateCache::default();
    candidates_for_candidate_expr_with_cache(
        snapshot,
        expr,
        max_candidates,
        deadline,
        &mut estimate_cache,
    )
}

fn candidates_for_candidate_expr_with_cache(
    snapshot: &IndexSnapshot,
    expr: &CandidateExpr,
    max_candidates: usize,
    deadline: Instant,
    estimate_cache: &mut CandidateEstimateCache,
) -> Result<Vec<DocId>, MemoryError> {
    check_deadline(deadline)?;
    let candidates = match expr {
        CandidateExpr::Seed(seed) => candidates_for_literal(
            snapshot,
            seed,
            LiteralCase::Sensitive,
            max_candidates,
            deadline,
        )?,
        CandidateExpr::And(children) => {
            for child in children {
                if candidate_expr_estimated_docs_with_cache(
                    snapshot,
                    child,
                    deadline,
                    estimate_cache,
                )? == 0
                {
                    return Ok(Vec::new());
                }
            }
            let mut child_sets = Vec::with_capacity(children.len());
            for child in candidate_children_by_selectivity_with_cache(
                snapshot,
                children,
                deadline,
                estimate_cache,
            )? {
                check_deadline(deadline)?;
                let child_set = candidates_for_candidate_expr_with_cache(
                    snapshot,
                    child,
                    max_candidates,
                    deadline,
                    estimate_cache,
                )?;
                if child_set.is_empty() {
                    return Ok(Vec::new());
                }
                child_sets.push(child_set);
            }
            intersect_candidate_sets(child_sets, deadline)?
        }
        CandidateExpr::Or(children) => {
            let mut candidates = Vec::new();
            for child in candidate_children_by_selectivity_with_cache(
                snapshot,
                children,
                deadline,
                estimate_cache,
            )? {
                check_deadline(deadline)?;
                candidates = union_postings(
                    candidates,
                    candidates_for_candidate_expr_with_cache(
                        snapshot,
                        child,
                        max_candidates,
                        deadline,
                        estimate_cache,
                    )?,
                    max_candidates,
                    deadline,
                )?;
            }
            candidates
        }
    };

    ensure_candidate_limit(candidates.len(), max_candidates)?;
    Ok(candidates)
}

#[cfg(test)]
fn candidate_children_by_selectivity<'a>(
    snapshot: &IndexSnapshot,
    children: &'a [CandidateExpr],
    deadline: Instant,
) -> Result<Vec<&'a CandidateExpr>, MemoryError> {
    let mut estimate_cache = CandidateEstimateCache::default();
    candidate_children_by_selectivity_with_cache(snapshot, children, deadline, &mut estimate_cache)
}

fn candidate_children_by_selectivity_with_cache<'a>(
    snapshot: &IndexSnapshot,
    children: &'a [CandidateExpr],
    deadline: Instant,
    estimate_cache: &mut CandidateEstimateCache,
) -> Result<Vec<&'a CandidateExpr>, MemoryError> {
    let mut ordered = Vec::with_capacity(children.len());
    for child in children {
        check_deadline(deadline)?;
        let estimate =
            candidate_expr_estimated_docs_with_cache(snapshot, child, deadline, estimate_cache)?;
        ordered.push((estimate, candidate_expr_seed_count(child), child));
    }
    ordered.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(right.2))
    });
    Ok(ordered.into_iter().map(|(_, _, child)| child).collect())
}

fn candidate_expr_estimated_docs_with_cache(
    snapshot: &IndexSnapshot,
    expr: &CandidateExpr,
    deadline: Instant,
    estimate_cache: &mut CandidateEstimateCache,
) -> Result<usize, MemoryError> {
    check_deadline(deadline)?;
    if let Some(estimate) = estimate_cache.expr_estimates.get(expr) {
        return Ok(*estimate);
    }

    let estimate = match expr {
        CandidateExpr::Seed(seed) => literal_candidate_estimate_with_cache(
            snapshot,
            seed,
            LiteralCase::Sensitive,
            deadline,
            estimate_cache,
        ),
        CandidateExpr::And(children) => {
            let mut estimate = None;
            for child in children {
                check_deadline(deadline)?;
                let child_estimate = candidate_expr_estimated_docs_with_cache(
                    snapshot,
                    child,
                    deadline,
                    estimate_cache,
                )?;
                if child_estimate == 0 {
                    estimate_cache.expr_estimates.insert(expr.clone(), 0);
                    return Ok(0);
                }
                estimate = Some(
                    estimate.map_or(child_estimate, |current: usize| current.min(child_estimate)),
                );
            }
            Ok(estimate.unwrap_or(0))
        }
        CandidateExpr::Or(children) => {
            let mut sum = 0_usize;
            for child in children {
                check_deadline(deadline)?;
                sum = sum.saturating_add(candidate_expr_estimated_docs_with_cache(
                    snapshot,
                    child,
                    deadline,
                    estimate_cache,
                )?);
            }
            Ok(sum)
        }
    }?;
    estimate_cache.expr_estimates.insert(expr.clone(), estimate);
    Ok(estimate)
}

fn candidate_expr_seed_count(expr: &CandidateExpr) -> usize {
    match expr {
        CandidateExpr::Seed(_) => 1,
        CandidateExpr::And(children) | CandidateExpr::Or(children) => {
            children.iter().map(candidate_expr_seed_count).sum()
        }
    }
}

fn plan_candidate_estimate_with_cache(
    snapshot: &IndexSnapshot,
    plan: &QueryPlan,
    limits: &Limits,
    deadline: Instant,
    estimate_cache: &mut CandidateEstimateCache,
) -> Result<usize, MemoryError> {
    check_deadline(deadline)?;
    match plan {
        QueryPlan::Exact { literal, case } => literal_candidate_estimate_with_cache(
            snapshot,
            literal,
            *case,
            deadline,
            estimate_cache,
        ),
        QueryPlan::ShortExact { .. } => {
            check_deadline(deadline)?;
            Ok(snapshot.documents.len())
        }
        QueryPlan::WordExact { literal, case } => literal_candidate_estimate_with_cache(
            snapshot,
            literal,
            *case,
            deadline,
            estimate_cache,
        ),
        QueryPlan::Regex { candidates, .. } => {
            candidate_expr_estimated_docs_with_cache(snapshot, candidates, deadline, estimate_cache)
        }
        QueryPlan::Fuzzy { .. } => {
            let mut fuzzy_candidate_cache = FuzzySeedCandidateCache::default();
            let selection = select_fuzzy_seed_plan_with_cache(
                snapshot,
                plan,
                limits,
                deadline,
                &mut fuzzy_candidate_cache,
            )?;
            Ok(selection.candidates.len())
        }
    }
}

fn select_fuzzy_seed_plan_for_query_with_cache(
    snapshot: &IndexSnapshot,
    plan: &QueryPlan,
    limits: &Limits,
    deadline: Instant,
    fuzzy_candidate_cache: &mut FuzzySeedCandidateCache,
) -> Result<Option<FuzzySeedSelection>, MemoryError> {
    if matches!(plan, QueryPlan::Fuzzy { .. }) {
        select_fuzzy_seed_plan_with_cache(snapshot, plan, limits, deadline, fuzzy_candidate_cache)
            .map(Some)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
fn select_fuzzy_seed_plan(
    snapshot: &IndexSnapshot,
    plan: &QueryPlan,
    limits: &Limits,
    deadline: Instant,
) -> Result<FuzzySeedSelection, MemoryError> {
    let mut fuzzy_candidate_cache = FuzzySeedCandidateCache::default();
    select_fuzzy_seed_plan_with_cache(snapshot, plan, limits, deadline, &mut fuzzy_candidate_cache)
}

fn select_fuzzy_seed_plan_with_cache(
    snapshot: &IndexSnapshot,
    plan: &QueryPlan,
    limits: &Limits,
    deadline: Instant,
    fuzzy_candidate_cache: &mut FuzzySeedCandidateCache,
) -> Result<FuzzySeedSelection, MemoryError> {
    let QueryPlan::Fuzzy { seed_plans, .. } = plan else {
        return Err(MemoryError::new(
            "internal_error",
            "internal_error",
            "fuzzy seed selection requires a fuzzy query plan",
        ));
    };

    let mut best: Option<(FuzzySeedPlanScore, FuzzySeedSelection)> = None;
    let mut saw_candidate_budget_exceeded = false;
    for seed_plan in seed_plans {
        check_deadline(deadline)?;
        let selection = match fuzzy_seed_selection_for_seed_plan(
            snapshot,
            seed_plan,
            seed_plans.len(),
            limits,
            deadline,
            fuzzy_candidate_cache,
        ) {
            Ok(selection) => selection,
            Err(err) if is_max_candidates_exceeded(&err) => {
                saw_candidate_budget_exceeded = true;
                continue;
            }
            Err(err) => return Err(err),
        };
        if selection.candidates.is_empty() {
            return Ok(selection);
        }
        let score = fuzzy_seed_plan_score(&selection);
        match &best {
            Some((best_score, _)) if *best_score <= score => {}
            _ => best = Some((score, selection)),
        }
    }

    if let Some((_, selection)) = best {
        return Ok(selection);
    }

    if saw_candidate_budget_exceeded {
        return Err(max_candidates_exceeded_error());
    }

    Err(MemoryError::new(
        "unsupported_search_option",
        "fuzzy_pattern_unseedable",
        "fuzzy fixed-string pattern cannot be partitioned into required seed segments",
    ))
}

fn fuzzy_seed_selection_for_seed_plan(
    snapshot: &IndexSnapshot,
    seed_plan: &FuzzySeedPlan,
    partition_count: usize,
    limits: &Limits,
    deadline: Instant,
    fuzzy_candidate_cache: &mut FuzzySeedCandidateCache,
) -> Result<FuzzySeedSelection, MemoryError> {
    let mut seed_sets = Vec::new();
    for seed in unique_fuzzy_candidate_seeds(&seed_plan.seeds, deadline)? {
        check_deadline(deadline)?;
        let candidates = fuzzy_seed_candidates_with_cache(
            snapshot,
            &seed,
            limits,
            deadline,
            fuzzy_candidate_cache,
        )?;
        seed_sets.push((seed, candidates));
    }

    seed_sets.sort_by(|left, right| {
        left.1
            .len()
            .cmp(&right.1.len())
            .then_with(|| right.0.len().cmp(&left.0.len()))
            .then_with(|| left.0.cmp(&right.0))
    });

    let seed_candidate_counts = seed_sets
        .iter()
        .map(|(_, candidates)| candidates.len())
        .collect::<Vec<_>>();
    let seed_byte_lengths = seed_sets
        .iter()
        .map(|(seed, _)| seed.len())
        .collect::<Vec<_>>();
    let candidate_seeds = seed_sets
        .iter()
        .map(|(seed, _)| seed.clone())
        .collect::<Vec<_>>();
    let duplicate_seed_count = seed_plan.seeds.len().saturating_sub(candidate_seeds.len());

    let mut candidates = Vec::new();
    for (_, seed_candidates) in seed_sets {
        check_deadline(deadline)?;
        candidates = union_postings(candidates, seed_candidates, limits.max_candidates, deadline)?;
    }

    Ok(FuzzySeedSelection {
        partition_count,
        partition_index: seed_plan.partition_index,
        candidate_seeds,
        verifier_seeds: seed_plan.verifier_seeds.clone(),
        candidates,
        duplicate_seed_count,
        seed_candidate_counts,
        seed_byte_lengths,
    })
}

fn fuzzy_seed_candidates_with_cache(
    snapshot: &IndexSnapshot,
    seed: &[u8],
    limits: &Limits,
    deadline: Instant,
    fuzzy_candidate_cache: &mut FuzzySeedCandidateCache,
) -> Result<Vec<DocId>, MemoryError> {
    check_deadline(deadline)?;
    if let Some(candidates) = fuzzy_candidate_cache.candidates_by_seed.get(seed) {
        return Ok(candidates.clone());
    }

    let candidates = candidates_for_literal(
        snapshot,
        seed,
        LiteralCase::Sensitive,
        limits.max_candidates,
        deadline,
    )?;
    fuzzy_candidate_cache
        .candidates_by_seed
        .insert(seed.to_vec(), candidates.clone());
    Ok(candidates)
}

fn unique_fuzzy_candidate_seeds(
    seeds: &[Vec<u8>],
    deadline: Instant,
) -> Result<Vec<Vec<u8>>, MemoryError> {
    let mut unique = seeds.to_vec();
    check_deadline(deadline)?;
    unique.sort();
    check_deadline(deadline)?;
    unique.dedup();
    check_deadline(deadline)?;
    Ok(unique)
}

fn fuzzy_seed_plan_score(selection: &FuzzySeedSelection) -> FuzzySeedPlanScore {
    FuzzySeedPlanScore {
        candidate_count: selection.candidates.len(),
        max_seed_candidate_count: selection
            .seed_candidate_counts
            .iter()
            .copied()
            .max()
            .unwrap_or_default(),
        candidate_seed_count: selection.candidate_seeds.len(),
        duplicate_seed_count: selection.duplicate_seed_count,
        shortest_seed_len: Reverse(
            selection
                .seed_byte_lengths
                .iter()
                .copied()
                .min()
                .unwrap_or_default(),
        ),
        longest_seed_len: Reverse(
            selection
                .seed_byte_lengths
                .iter()
                .copied()
                .max()
                .unwrap_or_default(),
        ),
        partition_index: selection.partition_index,
    }
}

fn is_max_candidates_exceeded(err: &MemoryError) -> bool {
    err.error_type == "resource_limit_exceeded" && err.fallback_reason == "max_candidates_exceeded"
}

fn max_candidates_exceeded_error() -> MemoryError {
    MemoryError::new(
        "resource_limit_exceeded",
        "max_candidates_exceeded",
        "memory search candidate limit exceeded",
    )
}

#[cfg(test)]
fn candidate_seeds_by_selectivity<'a>(
    snapshot: &IndexSnapshot,
    seeds: &'a [Vec<u8>],
    deadline: Instant,
) -> Result<Vec<&'a Vec<u8>>, MemoryError> {
    let mut ordered = Vec::with_capacity(seeds.len());
    for seed in seeds {
        check_deadline(deadline)?;
        ordered.push((
            literal_candidate_estimate(snapshot, seed, LiteralCase::Sensitive, deadline)?,
            seed.len(),
            seed,
        ));
    }
    ordered.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.as_slice().cmp(right.2.as_slice()))
    });
    Ok(ordered.into_iter().map(|(_, _, seed)| seed).collect())
}

fn intersect_candidate_sets(
    mut child_sets: Vec<Vec<DocId>>,
    deadline: Instant,
) -> Result<Vec<DocId>, MemoryError> {
    check_deadline(deadline)?;
    if child_sets.is_empty() {
        return Ok(Vec::new());
    }
    child_sets.sort_by_key(Vec::len);
    let mut result = child_sets.remove(0);
    for child in child_sets {
        check_deadline(deadline)?;
        result = intersect_doc_id_sets(&result, &child, deadline)?;
        if result.is_empty() {
            break;
        }
    }
    Ok(result)
}

fn union_postings(
    left: Vec<DocId>,
    right: Vec<DocId>,
    max_candidates: usize,
    deadline: Instant,
) -> Result<Vec<DocId>, MemoryError> {
    check_deadline(deadline)?;
    if left.is_empty() {
        ensure_candidate_limit(right.len(), max_candidates)?;
        return Ok(right);
    }
    if right.is_empty() {
        ensure_candidate_limit(left.len(), max_candidates)?;
        return Ok(left);
    }

    let mut merged = Vec::with_capacity(
        left.len()
            .saturating_add(right.len())
            .min(max_candidates.saturating_add(1)),
    );
    let mut left_index = 0;
    let mut right_index = 0;
    while left_index < left.len() && right_index < right.len() {
        check_deadline(deadline)?;
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Equal => {
                push_candidate(&mut merged, left[left_index], max_candidates)?;
                left_index += 1;
                right_index += 1;
            }
            std::cmp::Ordering::Less => {
                push_candidate(&mut merged, left[left_index], max_candidates)?;
                left_index += 1;
            }
            std::cmp::Ordering::Greater => {
                push_candidate(&mut merged, right[right_index], max_candidates)?;
                right_index += 1;
            }
        }
    }
    for &candidate in &left[left_index..] {
        check_deadline(deadline)?;
        push_candidate(&mut merged, candidate, max_candidates)?;
    }
    for &candidate in &right[right_index..] {
        check_deadline(deadline)?;
        push_candidate(&mut merged, candidate, max_candidates)?;
    }
    Ok(merged)
}

fn intersect_doc_id_sets(
    left: &[DocId],
    right: &[DocId],
    deadline: Instant,
) -> Result<Vec<DocId>, MemoryError> {
    check_deadline(deadline)?;
    let mut result = Vec::with_capacity(left.len().min(right.len()));
    let mut left_index = 0;
    let mut right_index = 0;
    while left_index < left.len() && right_index < right.len() {
        check_deadline(deadline)?;
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Equal => {
                result.push(left[left_index]);
                left_index += 1;
                right_index += 1;
            }
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
        }
    }
    Ok(result)
}

fn push_candidate(
    candidates: &mut Vec<DocId>,
    candidate: DocId,
    max_candidates: usize,
) -> Result<(), MemoryError> {
    candidates.push(candidate);
    ensure_candidate_limit(candidates.len(), max_candidates)
}

fn ensure_candidate_limit(
    candidate_count: usize,
    max_candidates: usize,
) -> Result<(), MemoryError> {
    if candidate_count > max_candidates {
        return Err(max_candidates_exceeded_error());
    }
    Ok(())
}

fn candidates_for_literal(
    snapshot: &IndexSnapshot,
    literal: &[u8],
    case: LiteralCase,
    max_candidates: usize,
    deadline: Instant,
) -> Result<Vec<DocId>, MemoryError> {
    let Some(posting_lists) = literal_postings_by_selectivity(snapshot, literal, case, deadline)?
    else {
        return Ok(Vec::new());
    };
    if posting_lists.is_empty() {
        return Ok(Vec::new());
    }
    if posting_lists.len() == 1 {
        let postings = posting_lists[0].postings;
        ensure_candidate_limit(postings.len(), max_candidates)?;
        return Ok(postings.to_vec());
    }

    let posting_lists: Vec<&Postings> = posting_lists
        .iter()
        .map(|literal_posting| literal_posting.postings)
        .collect();
    let candidates = intersect_postings(&posting_lists, deadline)?;
    ensure_candidate_limit(candidates.len(), max_candidates)?;
    Ok(candidates)
}

#[cfg(test)]
fn literal_candidate_estimate(
    snapshot: &IndexSnapshot,
    literal: &[u8],
    case: LiteralCase,
    deadline: Instant,
) -> Result<usize, MemoryError> {
    let mut estimate_cache = CandidateEstimateCache::default();
    literal_candidate_estimate_with_cache(snapshot, literal, case, deadline, &mut estimate_cache)
}

fn literal_candidate_estimate_with_cache(
    snapshot: &IndexSnapshot,
    literal: &[u8],
    case: LiteralCase,
    deadline: Instant,
    estimate_cache: &mut CandidateEstimateCache,
) -> Result<usize, MemoryError> {
    check_deadline(deadline)?;
    let key = (literal.to_vec(), case);
    if let Some(estimate) = estimate_cache.literal_estimates.get(&key) {
        return Ok(*estimate);
    }

    let Some(posting_lists) = literal_postings_by_selectivity(snapshot, literal, case, deadline)?
    else {
        estimate_cache.literal_estimates.insert(key, 0);
        return Ok(0);
    };
    let estimate = posting_lists
        .first()
        .map(|literal_posting| literal_posting.postings.document_frequency().get())
        .unwrap_or(0);
    estimate_cache.literal_estimates.insert(key, estimate);
    Ok(estimate)
}

fn literal_postings_by_selectivity<'a>(
    snapshot: &'a IndexSnapshot,
    literal: &[u8],
    case: LiteralCase,
    deadline: Instant,
) -> Result<Option<Vec<LiteralPosting<'a>>>, MemoryError> {
    check_deadline(deadline)?;
    let folded_literal;
    let (indexed_literal, index_postings) = match case {
        LiteralCase::Sensitive => (literal, &snapshot.postings),
        LiteralCase::AsciiInsensitive => {
            folded_literal = ascii_folded_bytes_with_deadline(literal, deadline)?;
            (folded_literal.as_slice(), &snapshot.ascii_folded_postings)
        }
    };

    let trigrams = literal_trigrams_with_deadline(indexed_literal, deadline)?;
    let mut posting_lists = Vec::with_capacity(trigrams.len());
    for trigram in trigrams {
        check_deadline(deadline)?;
        let Some(postings) = index_postings.get(&trigram) else {
            return Ok(None);
        };
        if postings.len() == 0 {
            return Ok(None);
        }
        posting_lists.push(LiteralPosting { trigram, postings });
    }
    posting_lists.sort_by(|left, right| {
        left.postings
            .document_frequency()
            .cmp(&right.postings.document_frequency())
            .then_with(|| left.trigram.cmp(&right.trigram))
    });
    Ok(Some(posting_lists))
}

fn candidates_for_short_literal_direct_scan(
    snapshot: &IndexSnapshot,
    literal: &[u8],
    case: LiteralCase,
    limits: &Limits,
    deadline: Instant,
) -> Result<Vec<DocId>, MemoryError> {
    check_deadline(deadline)?;
    let mut candidates = Vec::new();
    let mut scanned_lines = 0_usize;

    for (doc_index, doc) in snapshot.documents.iter().enumerate() {
        check_deadline(deadline)?;
        let mut doc_matches = false;
        for range in &doc.lines {
            check_deadline(deadline)?;
            record_short_literal_scan_line(
                &mut scanned_lines,
                limits.max_short_literal_scan_lines,
            )?;
            let line = &doc.content[range.start..range.end];
            let line_matches = match case {
                LiteralCase::Sensitive => contains_subslice(line, literal),
                LiteralCase::AsciiInsensitive => {
                    contains_subslice_ascii_case_insensitive(line, literal)
                }
            };
            if line_matches {
                doc_matches = true;
                break;
            }
        }
        if doc_matches {
            push_candidate(
                &mut candidates,
                DocId::from_index(doc_index)?,
                limits.max_candidates,
            )?;
        }
    }

    Ok(candidates)
}

fn record_short_literal_scan_line(
    scanned_lines: &mut usize,
    max_lines: usize,
) -> Result<(), MemoryError> {
    *scanned_lines = scanned_lines.checked_add(1).ok_or_else(|| {
        MemoryError::new(
            "resource_limit_exceeded",
            "max_short_literal_scan_lines_exceeded",
            "short fixed-string scan line count overflowed",
        )
    })?;
    if *scanned_lines > max_lines {
        return Err(MemoryError::new(
            "resource_limit_exceeded",
            "max_short_literal_scan_lines_exceeded",
            "short fixed-string scan line limit exceeded",
        ));
    }
    Ok(())
}

fn verify_and_render(
    snapshot: &IndexSnapshot,
    candidates: &[DocId],
    plan: &QueryPlan,
    fuzzy_seed_selection: Option<&FuzzySeedSelection>,
    req: &NormalizedSearchRequest,
    limits: &Limits,
    deadline: Instant,
) -> Result<(Vec<SearchEvent>, bool, VerificationStats, BTreeSet<DocId>), MemoryError> {
    let mut events = Vec::with_capacity(req.max_results().min(256));
    let mut result_doc_ids = BTreeSet::new();
    let mut truncated = false;
    let mut verification_stats = VerificationStats::default();
    let max_results = req.max_results();
    let context = req.context();
    let cancel_token = current_cancellation_token();
    let cancel = cancel_token.as_ref();

    'docs: for &doc_id in candidates {
        check_cancellation(cancel)?;
        check_deadline(deadline)?;
        let Some(doc) = snapshot.documents.get(doc_id.to_index()) else {
            return Err(MemoryError::new(
                "search_index_incomplete",
                "invalid_doc_id",
                "memory search candidate referenced a missing indexed document",
            ));
        };
        let remaining_event_budget = max_results.saturating_sub(events.len());
        if remaining_event_budget == 0 {
            truncated = true;
            break 'docs;
        }
        let matched = matching_line_indexes_with_budget(
            doc,
            plan,
            fuzzy_seed_selection,
            limits,
            deadline,
            &mut verification_stats,
            Some(LineMatchBudget {
                context,
                event_budget: remaining_event_budget,
            }),
        )?;
        let matched_lines = matched.lines;
        if matched_lines.is_empty() {
            continue;
        }

        check_deadline(deadline)?;
        let event_count_before_doc = events.len();
        let doc_truncated = push_rendered_events(
            &mut events,
            doc,
            &matched_lines,
            context,
            max_results,
            deadline,
        )?;
        if events.len() > event_count_before_doc {
            result_doc_ids.insert(doc_id);
        }
        if doc_truncated {
            truncated = true;
            break 'docs;
        }
        if matched.stopped_after_budget {
            truncated = true;
            break 'docs;
        }
    }

    Ok((events, truncated, verification_stats, result_doc_ids))
}

#[derive(Clone, Copy, Debug, Default)]
struct VerificationStats {
    verified_lines: usize,
    fuzzy_verified_lines: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FreshnessValidationStats {
    result_files_checked: usize,
    indexed_files_checked: usize,
    directories_checked: usize,
    full_scope_scans: usize,
}

impl FreshnessValidationStats {
    fn combine(self, other: Self) -> Self {
        Self {
            result_files_checked: self
                .result_files_checked
                .saturating_add(other.result_files_checked),
            indexed_files_checked: self
                .indexed_files_checked
                .saturating_add(other.indexed_files_checked),
            directories_checked: self
                .directories_checked
                .saturating_add(other.directories_checked),
            full_scope_scans: self.full_scope_scans.saturating_add(other.full_scope_scans),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotValidationScope {
    TargetedResultFiles,
    FullScope,
}

impl SnapshotValidationScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::TargetedResultFiles => "targeted_result_files",
            Self::FullScope => "full_scope",
        }
    }
}

#[derive(Clone, Debug)]
struct SnapshotValidation<'a> {
    req: &'a NormalizedSearchRequest,
    result_doc_ids: BTreeSet<DocId>,
}

impl<'a> SnapshotValidation<'a> {
    fn targeted(req: &'a NormalizedSearchRequest, result_doc_ids: BTreeSet<DocId>) -> Self {
        Self {
            req,
            result_doc_ids,
        }
    }

    fn validate(
        self,
        snapshot: &IndexSnapshot,
        force_full_scope: bool,
        deadline: Instant,
    ) -> Result<FreshnessValidationResult, MemoryError> {
        let required_full_scope_reason = if force_full_scope {
            Some("validation_interval")
        } else if !self.req.no_ignore() && force_full_scope_on_ignore_enabled() {
            Some("ignore_rules_forced_full_scope")
        } else {
            None
        };

        if let Some(reason) = required_full_scope_reason {
            let stats = check_snapshot_fresh(self.req, snapshot, deadline)?;
            return Ok(FreshnessValidationResult::verified_full_scope(
                stats,
                Some(reason),
            ));
        }

        if !self.req.no_ignore()
            && let Some(reason) = check_ignore_fingerprint(
                self.req,
                snapshot,
                snapshot.ignore_fingerprint.as_ref(),
                deadline,
            )?
        {
            let stats = check_snapshot_fresh(self.req, snapshot, deadline)?;
            return Ok(FreshnessValidationResult::verified_full_scope(
                stats,
                Some(reason),
            ));
        }

        match check_targeted_snapshot_fresh(snapshot, &self.result_doc_ids, deadline)? {
            TargetedFreshnessOutcome::Verified(stats) => {
                Ok(FreshnessValidationResult::verified_targeted(stats))
            }
            TargetedFreshnessOutcome::NeedsFullScope { reason, stats } => {
                let full_stats = check_snapshot_fresh(self.req, snapshot, deadline)?;
                Ok(FreshnessValidationResult::verified_full_scope(
                    stats.combine(full_stats),
                    Some(reason),
                ))
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FreshnessValidationResult {
    status: &'static str,
    scope: SnapshotValidationScope,
    state: &'static str,
    index_state: IndexEntryState,
    stats: FreshnessValidationStats,
    full_scan_reason: Option<&'static str>,
}

impl FreshnessValidationResult {
    fn verified_targeted(stats: FreshnessValidationStats) -> Self {
        Self {
            status: "verified",
            scope: SnapshotValidationScope::TargetedResultFiles,
            state: "targeted_verified",
            index_state: IndexEntryState::Ready,
            stats,
            full_scan_reason: None,
        }
    }

    fn verified_full_scope(
        stats: FreshnessValidationStats,
        full_scan_reason: Option<&'static str>,
    ) -> Self {
        Self {
            status: "verified",
            scope: SnapshotValidationScope::FullScope,
            state: "full_scope_verified",
            index_state: IndexEntryState::Ready,
            stats,
            full_scan_reason,
        }
    }

    fn full_scope_ran(&self) -> bool {
        self.stats.full_scope_scans > 0
    }
}

fn validate_cached_snapshot_fresh(
    key: &IndexKey,
    snapshot: &Arc<IndexSnapshot>,
    validation: SnapshotValidation<'_>,
    deadline: Instant,
) -> Result<FreshnessValidationResult, MemoryError> {
    let force_full_scope = {
        let mut manager = lock_index_manager();
        manager.begin_validation(key, snapshot)
    };

    match validation.validate(snapshot, force_full_scope, deadline) {
        Ok(mut result) => {
            result.index_state =
                lock_index_manager().complete_validation(key, snapshot, result.full_scope_ran());
            Ok(result)
        }
        Err(err) => {
            lock_index_manager().record_validation_failure(key, snapshot, &err);
            Err(err)
        }
    }
}

enum TargetedFreshnessOutcome {
    Verified(FreshnessValidationStats),
    NeedsFullScope {
        reason: &'static str,
        stats: FreshnessValidationStats,
    },
}

fn check_targeted_snapshot_fresh(
    snapshot: &IndexSnapshot,
    result_doc_ids: &BTreeSet<DocId>,
    deadline: Instant,
) -> Result<TargetedFreshnessOutcome, MemoryError> {
    let mut stats = FreshnessValidationStats::default();
    if let Some(reason) =
        check_scope_directory_fingerprints(&snapshot.scope_fingerprint, &mut stats, deadline)?
    {
        return Ok(TargetedFreshnessOutcome::NeedsFullScope { reason, stats });
    }

    for (doc_index, doc) in snapshot.documents.iter().enumerate() {
        check_deadline(deadline)?;
        let doc_id = DocId::from_index(doc_index)?;
        let is_result_file = result_doc_ids.contains(&doc_id);
        stats.indexed_files_checked = stats.indexed_files_checked.saturating_add(1);
        if is_result_file {
            stats.result_files_checked = stats.result_files_checked.saturating_add(1);
        }

        let metadata = match fs::metadata(&doc.path) {
            Ok(metadata) => metadata,
            Err(err) if is_result_file => {
                return Err(file_changed_error(
                    format!(
                        "failed to re-read metadata for result file {}: {err}",
                        doc.path.display()
                    ),
                    "file_changed_during_verification",
                ));
            }
            Err(_) => {
                return Ok(TargetedFreshnessOutcome::NeedsFullScope {
                    reason: "indexed_file_missing",
                    stats,
                });
            }
        };
        check_deadline(deadline)?;
        if file_metadata_matches_without_hash(&doc.stamp, &metadata) {
            continue;
        }

        if !is_result_file {
            return Ok(TargetedFreshnessOutcome::NeedsFullScope {
                reason: "indexed_file_metadata_changed",
                stats,
            });
        }

        validate_result_file_content_matches(doc, &metadata, deadline)?;
    }

    Ok(TargetedFreshnessOutcome::Verified(stats))
}

fn check_scope_directory_fingerprints(
    scope_fingerprint: &ScopeFingerprint,
    stats: &mut FreshnessValidationStats,
    deadline: Instant,
) -> Result<Option<&'static str>, MemoryError> {
    for directory in &scope_fingerprint.directories {
        check_deadline(deadline)?;
        stats.directories_checked = stats.directories_checked.saturating_add(1);
        let metadata = match fs::metadata(&directory.path) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(Some("directory_set_changed")),
        };
        check_deadline(deadline)?;
        if !metadata_stamp_matches(&directory.stamp, &metadata) {
            return Ok(Some("directory_set_changed"));
        }
    }

    Ok(None)
}

fn check_ignore_fingerprint(
    req: &NormalizedSearchRequest,
    snapshot: &IndexSnapshot,
    expected: Option<&IgnoreFingerprint>,
    deadline: Instant,
) -> Result<Option<&'static str>, MemoryError> {
    ignore_fingerprint_change_reason(
        Path::new(req.root()),
        snapshot
            .scope_fingerprint
            .directories
            .iter()
            .map(|entry| entry.path.as_path()),
        req.no_ignore(),
        expected,
        deadline,
    )
    .map_err(|err| match err {
        crate::tools::scope_cache::ScopeCacheError::Timeout => MemoryError::timeout(),
        crate::tools::scope_cache::ScopeCacheError::Walk(message) => MemoryError::new(
            "search_index_incomplete",
            "walk_error",
            format!("memory search ignore fingerprint failed: {message}"),
        ),
        crate::tools::scope_cache::ScopeCacheError::Io(io_err) => MemoryError::new(
            "search_index_incomplete",
            "metadata_error",
            format!("memory search ignore fingerprint failed: {io_err}"),
        ),
    })
}

fn check_snapshot_fresh(
    req: &NormalizedSearchRequest,
    snapshot: &IndexSnapshot,
    deadline: Instant,
) -> Result<FreshnessValidationStats, MemoryError> {
    let mut stats = FreshnessValidationStats {
        full_scope_scans: 1,
        ..FreshnessValidationStats::default()
    };
    let current_paths = discover_files(req, Some(deadline))?;
    check_deadline(deadline)?;
    let expected_paths: BTreeSet<PathBuf> =
        snapshot.documents.iter().map(|d| d.path.clone()).collect();
    let observed_paths: BTreeSet<PathBuf> = current_paths.into_iter().collect();
    if expected_paths != observed_paths {
        return Err(file_changed_error(
            "file set changed during memory search verification",
            "file_set_changed",
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
        check_deadline(deadline)?;
        stats.indexed_files_checked = stats.indexed_files_checked.saturating_add(1);
        if file_metadata_matches_without_hash(&doc.stamp, &metadata) {
            continue;
        }

        validate_result_file_content_matches(doc, &metadata, deadline)?;
    }

    Ok(stats)
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

fn literal_trigrams_with_deadline(
    bytes: &[u8],
    deadline: Instant,
) -> Result<Vec<[u8; 3]>, MemoryError> {
    check_deadline(deadline)?;
    let mut trigrams = Vec::new();
    let mut seen = HashSet::new();
    for (index, window) in bytes.windows(3).enumerate() {
        if index.is_multiple_of(TRIGRAM_DEADLINE_CHECK_STRIDE) {
            check_deadline(deadline)?;
        }
        let trigram = [window[0], window[1], window[2]];
        if seen.insert(trigram) {
            trigrams.push(trigram);
        }
    }
    check_deadline(deadline)?;
    Ok(trigrams)
}

fn ascii_folded_bytes_with_deadline(
    bytes: &[u8],
    deadline: Instant,
) -> Result<Vec<u8>, MemoryError> {
    check_deadline(deadline)?;
    let mut folded = Vec::with_capacity(bytes.len());
    for (index, byte) in bytes.iter().enumerate() {
        if index.is_multiple_of(TRIGRAM_DEADLINE_CHECK_STRIDE) {
            check_deadline(deadline)?;
        }
        folded.push(byte.to_ascii_lowercase());
    }
    check_deadline(deadline)?;
    Ok(folded)
}

fn index_document_trigrams_with_deadline(
    doc_id: DocId,
    content: &[u8],
    postings: &mut HashMap<[u8; 3], Vec<DocId>>,
    ascii_folded_postings: &mut HashMap<[u8; 3], Vec<DocId>>,
    deadline: Instant,
) -> Result<(), MemoryError> {
    check_deadline(deadline)?;
    let mut sensitive_seen = HashSet::new();
    let mut folded_seen = HashSet::new();
    for (index, window) in content.windows(3).enumerate() {
        if index.is_multiple_of(TRIGRAM_DEADLINE_CHECK_STRIDE) {
            check_deadline(deadline)?;
        }

        let trigram = [window[0], window[1], window[2]];
        if sensitive_seen.insert(trigram) {
            postings.entry(trigram).or_default().push(doc_id);
        }

        let folded = [
            window[0].to_ascii_lowercase(),
            window[1].to_ascii_lowercase(),
            window[2].to_ascii_lowercase(),
        ];
        if folded_seen.insert(folded) {
            ascii_folded_postings
                .entry(folded)
                .or_default()
                .push(doc_id);
        }
    }
    check_deadline(deadline)?;
    Ok(())
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

fn required_candidate_expr(
    hir: &Hir,
    deadline: Instant,
) -> Result<Option<CandidateExpr>, MemoryError> {
    check_deadline(deadline)?;
    let expr = match hir.kind() {
        HirKind::Empty | HirKind::Class(_) | HirKind::Look(_) => None,
        HirKind::Literal(literal) => candidate_seed(literal.0.as_ref()),
        HirKind::Capture(capture) => required_candidate_expr(capture.sub.as_ref(), deadline)?,
        HirKind::Repetition(repetition) => {
            if repetition.min == 0 {
                None
            } else {
                required_candidate_expr(repetition.sub.as_ref(), deadline)?
            }
        }
        HirKind::Concat(parts) => {
            let mut exprs = Vec::new();
            for part in parts {
                check_deadline(deadline)?;
                if let Some(expr) = required_candidate_expr(part, deadline)? {
                    exprs.push(expr);
                }
            }
            candidate_and(exprs)
        }
        HirKind::Alternation(parts) => {
            let mut alternatives = Vec::with_capacity(parts.len());
            for part in parts {
                check_deadline(deadline)?;
                let Some(expr) = required_candidate_expr(part, deadline)? else {
                    return Ok(None);
                };
                alternatives.push(expr);
            }
            candidate_or(alternatives)
        }
    };
    if expr.is_some() {
        return Ok(expr);
    }

    finite_literal_candidate_expr(hir, deadline)
}

fn finite_literal_candidate_expr(
    hir: &Hir,
    deadline: Instant,
) -> Result<Option<CandidateExpr>, MemoryError> {
    let Some(literals) = finite_literal_prefixes(hir, deadline)? else {
        return Ok(None);
    };
    let mut candidates = Vec::with_capacity(literals.len());
    for literal in literals {
        check_deadline(deadline)?;
        let Some(candidate) = candidate_seed(&literal) else {
            return Ok(None);
        };
        candidates.push(candidate);
    }
    Ok(candidate_or(candidates))
}

fn finite_literal_prefixes(
    hir: &Hir,
    deadline: Instant,
) -> Result<Option<Vec<Vec<u8>>>, MemoryError> {
    check_deadline(deadline)?;
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => Ok(Some(vec![Vec::new()])),
        HirKind::Literal(literal) => Ok(Some(vec![bounded_literal_prefix(literal.0.as_ref())])),
        HirKind::Class(_) => Ok(None),
        HirKind::Capture(capture) => finite_literal_prefixes(capture.sub.as_ref(), deadline),
        HirKind::Repetition(repetition) => finite_repetition_literal_prefixes(repetition, deadline),
        HirKind::Concat(parts) => finite_concat_literal_prefixes(parts, deadline),
        HirKind::Alternation(parts) => finite_alternation_literal_prefixes(parts, deadline),
    }
}

fn finite_repetition_literal_prefixes(
    repetition: &regex_syntax::hir::Repetition,
    deadline: Instant,
) -> Result<Option<Vec<Vec<u8>>>, MemoryError> {
    let Some(max) = repetition.max.and_then(|max| usize::try_from(max).ok()) else {
        return Ok(None);
    };
    let Ok(min) = usize::try_from(repetition.min) else {
        return Ok(None);
    };
    if max > MAX_REGEX_FINITE_LITERAL_REPEAT_COUNT {
        return Ok(None);
    }
    let Some(unit) = finite_literal_prefixes(repetition.sub.as_ref(), deadline)? else {
        return Ok(None);
    };

    let mut repeated = Vec::new();
    for count in min..=max {
        check_deadline(deadline)?;
        let Some(literals) = repeat_literal_prefix_set(&unit, count, deadline)? else {
            return Ok(None);
        };
        for literal in literals {
            if !push_limited_literal_prefix(&mut repeated, literal) {
                return Ok(None);
            }
        }
    }
    Ok(Some(repeated))
}

fn finite_concat_literal_prefixes(
    parts: &[Hir],
    deadline: Instant,
) -> Result<Option<Vec<Vec<u8>>>, MemoryError> {
    let mut prefixes = vec![Vec::new()];
    for part in parts {
        check_deadline(deadline)?;
        let Some(part_prefixes) = finite_literal_prefixes(part, deadline)? else {
            return Ok(None);
        };
        let Some(combined) = concat_literal_prefix_sets(&prefixes, &part_prefixes, deadline)?
        else {
            return Ok(None);
        };
        prefixes = combined;
    }
    Ok(Some(prefixes))
}

fn finite_alternation_literal_prefixes(
    parts: &[Hir],
    deadline: Instant,
) -> Result<Option<Vec<Vec<u8>>>, MemoryError> {
    let mut alternatives = Vec::new();
    for part in parts {
        check_deadline(deadline)?;
        let Some(part_literals) = finite_literal_prefixes(part, deadline)? else {
            return Ok(None);
        };
        for literal in part_literals {
            if !push_limited_literal_prefix(&mut alternatives, literal) {
                return Ok(None);
            }
        }
    }
    Ok(Some(alternatives))
}

fn repeat_literal_prefix_set(
    unit: &[Vec<u8>],
    count: usize,
    deadline: Instant,
) -> Result<Option<Vec<Vec<u8>>>, MemoryError> {
    let mut repeated = vec![Vec::new()];
    for _ in 0..count {
        check_deadline(deadline)?;
        let Some(combined) = concat_literal_prefix_sets(&repeated, unit, deadline)? else {
            return Ok(None);
        };
        repeated = combined;
    }
    Ok(Some(repeated))
}

fn concat_literal_prefix_sets(
    left: &[Vec<u8>],
    right: &[Vec<u8>],
    deadline: Instant,
) -> Result<Option<Vec<Vec<u8>>>, MemoryError> {
    let mut combined = Vec::new();
    for left_literal in left {
        for right_literal in right {
            check_deadline(deadline)?;
            if !push_limited_literal_prefix(
                &mut combined,
                concat_bounded_literal_prefix(left_literal, right_literal),
            ) {
                return Ok(None);
            }
        }
    }
    Ok(Some(combined))
}

fn concat_bounded_literal_prefix(left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut combined = Vec::with_capacity(
        left.len()
            .saturating_add(right.len())
            .min(MAX_REGEX_FINITE_LITERAL_BYTES),
    );
    combined.extend_from_slice(&left[..left.len().min(MAX_REGEX_FINITE_LITERAL_BYTES)]);
    let remaining = MAX_REGEX_FINITE_LITERAL_BYTES.saturating_sub(combined.len());
    if remaining > 0 {
        combined.extend_from_slice(&right[..right.len().min(remaining)]);
    }
    combined
}

fn bounded_literal_prefix(bytes: &[u8]) -> Vec<u8> {
    bytes[..bytes.len().min(MAX_REGEX_FINITE_LITERAL_BYTES)].to_vec()
}

fn push_limited_literal_prefix(literals: &mut Vec<Vec<u8>>, literal: Vec<u8>) -> bool {
    if literals.iter().any(|existing| existing == &literal) {
        return true;
    }
    if literals.len() >= MAX_REGEX_FINITE_LITERAL_ALTERNATIVES {
        return false;
    }
    literals.push(literal);
    true
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
    dedup_candidate_exprs(&mut exprs);
    match exprs.len() {
        0 => None,
        1 => exprs.pop(),
        _ => Some(wrap(exprs)),
    }
}

fn dedup_candidate_exprs(exprs: &mut Vec<CandidateExpr>) {
    let mut unique = Vec::with_capacity(exprs.len());
    for expr in exprs.drain(..) {
        if !unique.iter().any(|existing| existing == &expr) {
            unique.push(expr);
        }
    }
    *exprs = unique;
}

fn intersect_postings(
    posting_lists: &[&Postings],
    deadline: Instant,
) -> Result<Vec<DocId>, MemoryError> {
    check_deadline(deadline)?;
    if posting_lists.is_empty() {
        return Ok(Vec::new());
    }
    if posting_lists.iter().any(|postings| postings.len() == 0) {
        return Ok(Vec::new());
    }

    check_deadline(deadline)?;
    let mut result = posting_lists[0].to_vec();
    for postings in &posting_lists[1..] {
        check_deadline(deadline)?;
        let mut left = 0;
        let mut right = 0;
        let mut write = 0;
        let mut comparisons = 0_usize;
        while left < result.len() && right < postings.len() {
            if comparisons.is_multiple_of(POSTINGS_DEADLINE_CHECK_STRIDE) {
                check_deadline(deadline)?;
            }
            comparisons = comparisons.saturating_add(1);
            match result[left].cmp(&postings.doc_id_at(right)) {
                std::cmp::Ordering::Equal => {
                    result[write] = result[left];
                    write += 1;
                    left += 1;
                    right += 1;
                }
                std::cmp::Ordering::Less => left += 1,
                std::cmp::Ordering::Greater => right += 1,
            }
        }
        result.truncate(write);
        if result.is_empty() {
            break;
        }
    }
    check_deadline(deadline)?;
    Ok(result)
}

#[cfg(test)]
fn matching_line_indexes(
    doc: &Document,
    plan: &QueryPlan,
    fuzzy_seed_selection: Option<&FuzzySeedSelection>,
    limits: &Limits,
    deadline: Instant,
    verification_stats: &mut VerificationStats,
) -> Result<BTreeSet<usize>, MemoryError> {
    Ok(matching_line_indexes_with_budget(
        doc,
        plan,
        fuzzy_seed_selection,
        limits,
        deadline,
        verification_stats,
        None,
    )?
    .lines)
}

#[derive(Clone, Copy, Debug)]
struct LineMatchBudget {
    context: usize,
    event_budget: usize,
}

#[derive(Debug)]
struct MatchingLineOutcome {
    lines: BTreeSet<usize>,
    stopped_after_budget: bool,
}

fn matching_line_indexes_with_budget(
    doc: &Document,
    plan: &QueryPlan,
    fuzzy_seed_selection: Option<&FuzzySeedSelection>,
    limits: &Limits,
    deadline: Instant,
    verification_stats: &mut VerificationStats,
    budget: Option<LineMatchBudget>,
) -> Result<MatchingLineOutcome, MemoryError> {
    let mut matched = BTreeSet::new();
    let mut budget_saturated_scan_until = None;
    for (line_index, range) in doc.lines.iter().enumerate() {
        if line_index.is_multiple_of(LINE_VERIFY_DEADLINE_CHECK_STRIDE) {
            check_deadline(deadline)?;
        }
        verification_stats.verified_lines = verification_stats.verified_lines.saturating_add(1);
        let line = &doc.content[range.start..range.end];
        let is_match = match plan {
            QueryPlan::Exact { literal, case } | QueryPlan::ShortExact { literal, case } => {
                match case {
                    LiteralCase::Sensitive => contains_subslice(line, literal),
                    LiteralCase::AsciiInsensitive => {
                        contains_subslice_ascii_case_insensitive(line, literal)
                    }
                }
            }
            QueryPlan::WordExact { literal, case } => match case {
                LiteralCase::Sensitive => contains_word_subslice(line, literal),
                LiteralCase::AsciiInsensitive => {
                    contains_word_subslice_ascii_case_insensitive(line, literal)
                }
            },
            QueryPlan::Regex { matcher, .. } => matcher.is_match(line),
            QueryPlan::Fuzzy {
                pattern_chars,
                distance,
                ..
            } => {
                let verifier_seeds = fuzzy_seed_selection
                    .map(|selection| selection.verifier_seeds.as_slice())
                    .ok_or_else(|| {
                        MemoryError::new(
                            "internal_error",
                            "internal_error",
                            "fuzzy verifier requires a selected seed plan",
                        )
                    })?;
                verification_stats.fuzzy_verified_lines = verification_stats
                    .fuzzy_verified_lines
                    .checked_add(1)
                    .ok_or_else(|| {
                        MemoryError::new(
                            "resource_limit_exceeded",
                            "max_fuzzy_verified_lines_exceeded",
                            "fuzzy verifier line count overflowed",
                        )
                    })?;
                if verification_stats.fuzzy_verified_lines > limits.max_fuzzy_verified_lines {
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
                check_deadline(deadline)?;
                if char_count_exceeds(line, limits.max_fuzzy_line_chars, deadline)? {
                    return Err(MemoryError::new(
                        "resource_limit_exceeded",
                        "max_fuzzy_line_chars_exceeded",
                        "fuzzy verifier line length limit exceeded",
                    ));
                }
                fuzzy_line_matches_with_seeds(
                    line,
                    pattern_chars,
                    *distance,
                    verifier_seeds,
                    deadline,
                )?
            }
        };
        if is_match {
            matched.insert(line_index);
        }
        if let Some(scan_until) = budget_saturated_scan_until
            && line_index >= scan_until
        {
            return Ok(MatchingLineOutcome {
                lines: matched,
                stopped_after_budget: true,
            });
        }
        if (is_match || budget_saturated_scan_until.is_some())
            && let Some(budget) = budget
            && budget.event_budget > 0
            && !matched.is_empty()
        {
            let render_lines = render_line_indexes_with_deadline(
                &matched,
                doc.lines.len(),
                budget.context,
                deadline,
            )?;
            if render_lines.len() >= budget.event_budget {
                let scan_until = render_lines[budget.event_budget - 1];
                if line_index >= scan_until {
                    return Ok(MatchingLineOutcome {
                        lines: matched,
                        stopped_after_budget: true,
                    });
                }
                budget_saturated_scan_until = Some(scan_until);
            }
        }
    }
    check_deadline(deadline)?;
    Ok(MatchingLineOutcome {
        lines: matched,
        stopped_after_budget: false,
    })
}

#[derive(Clone, Debug)]
struct FuzzySeedPartition {
    byte_offsets: Vec<usize>,
    ranges: Vec<(usize, usize)>,
}

#[cfg(test)]
fn fuzzy_seed_segments(pattern: &str, distance: u8) -> Option<Vec<Vec<u8>>> {
    let partition = fuzzy_seed_partition_inner(pattern, distance, None)
        .ok()
        .flatten()?;
    Some(fuzzy_candidate_seeds_from_partition(pattern, &partition))
}

fn fuzzy_seed_partitions_with_deadline(
    pattern: &str,
    distance: u8,
    deadline: Instant,
) -> Result<Vec<FuzzySeedPartition>, MemoryError> {
    fuzzy_seed_partitions_inner(pattern, distance, Some(deadline))
}

#[cfg(test)]
fn fuzzy_seed_partition_inner(
    pattern: &str,
    distance: u8,
    deadline: Option<Instant>,
) -> Result<Option<FuzzySeedPartition>, MemoryError> {
    Ok(fuzzy_seed_partitions_inner(pattern, distance, deadline)?
        .into_iter()
        .next())
}

fn fuzzy_seed_partitions_inner(
    pattern: &str,
    distance: u8,
    deadline: Option<Instant>,
) -> Result<Vec<FuzzySeedPartition>, MemoryError> {
    let segment_count = usize::from(distance) + 1;
    let byte_offsets = pattern_byte_offsets_with_optional_deadline(pattern, deadline)?;
    let scalar_count = byte_offsets.len().saturating_sub(1);
    if scalar_count < segment_count {
        return Ok(Vec::new());
    }

    let mut ranges = Vec::with_capacity(segment_count);
    let mut partitions = Vec::new();
    collect_partition_seed_ranges(
        &byte_offsets,
        0,
        segment_count,
        &mut ranges,
        &mut partitions,
        deadline,
    )?;
    Ok(partitions)
}

fn pattern_byte_offsets_with_optional_deadline(
    pattern: &str,
    deadline: Option<Instant>,
) -> Result<Vec<usize>, MemoryError> {
    let mut byte_offsets = Vec::with_capacity(pattern.len().saturating_add(1));
    for (offset, _) in pattern.char_indices() {
        if let Some(deadline) = deadline {
            check_deadline(deadline)?;
        }
        byte_offsets.push(offset);
    }
    byte_offsets.push(pattern.len());
    Ok(byte_offsets)
}

fn fuzzy_candidate_seeds_from_partition(
    pattern: &str,
    partition: &FuzzySeedPartition,
) -> Vec<Vec<u8>> {
    partition
        .ranges
        .iter()
        .map(|(start, end)| {
            pattern.as_bytes()[partition.byte_offsets[*start]..partition.byte_offsets[*end]]
                .to_vec()
        })
        .collect()
}

fn fuzzy_verifier_seeds_from_partition(
    pattern_chars: &[char],
    partition: &FuzzySeedPartition,
) -> Vec<FuzzyVerifierSeed> {
    partition
        .ranges
        .iter()
        .map(|(start, end)| FuzzyVerifierSeed {
            pattern_start: *start,
            chars: pattern_chars[*start..*end].to_vec(),
        })
        .collect()
}

#[cfg(test)]
fn fuzzy_verifier_seeds_from_chars(
    pattern_chars: &[char],
    distance: usize,
    deadline: Instant,
) -> Result<Vec<FuzzyVerifierSeed>, MemoryError> {
    let distance = u8::try_from(distance).map_err(|_| {
        MemoryError::new(
            "unsupported_search_option",
            "unsupported_fuzzy_mode",
            "memory search supports fuzzy distances from 1 through 4",
        )
    })?;
    let pattern: String = pattern_chars.iter().copied().collect();
    let partition =
        fuzzy_seed_partition_inner(&pattern, distance, Some(deadline))?.ok_or_else(|| {
            MemoryError::new(
                "unsupported_search_option",
                "fuzzy_pattern_unseedable",
                "fuzzy fixed-string pattern cannot be partitioned into required seed segments",
            )
        })?;
    Ok(fuzzy_verifier_seeds_from_partition(
        pattern_chars,
        &partition,
    ))
}

fn collect_partition_seed_ranges(
    byte_offsets: &[usize],
    start_scalar: usize,
    remaining_segments: usize,
    ranges: &mut Vec<(usize, usize)>,
    partitions: &mut Vec<FuzzySeedPartition>,
    deadline: Option<Instant>,
) -> Result<(), MemoryError> {
    if let Some(deadline) = deadline {
        check_deadline(deadline)?;
    }
    if partitions.len() >= MAX_FUZZY_SEED_PARTITION_PLANS {
        return Ok(());
    }
    if remaining_segments == 1 {
        let end_scalar = byte_offsets.len() - 1;
        if seed_byte_len(byte_offsets, start_scalar, end_scalar) >= 3 {
            ranges.push((start_scalar, end_scalar));
            partitions.push(FuzzySeedPartition {
                byte_offsets: byte_offsets.to_vec(),
                ranges: ranges.clone(),
            });
            ranges.pop();
        }
        return Ok(());
    }

    for end_scalar in ordered_seed_end_scalars(byte_offsets, start_scalar, remaining_segments) {
        if let Some(deadline) = deadline {
            check_deadline(deadline)?;
        }
        ranges.push((start_scalar, end_scalar));
        collect_partition_seed_ranges(
            byte_offsets,
            end_scalar,
            remaining_segments - 1,
            ranges,
            partitions,
            deadline,
        )?;
        ranges.pop();
        if partitions.len() >= MAX_FUZZY_SEED_PARTITION_PLANS {
            break;
        }
    }
    Ok(())
}

fn ordered_seed_end_scalars(
    byte_offsets: &[usize],
    start_scalar: usize,
    remaining_segments: usize,
) -> Vec<usize> {
    let total_end = byte_offsets.len() - 1;
    let max_end = total_end.saturating_sub(remaining_segments - 1);
    let total_remaining_bytes = byte_offsets[total_end] - byte_offsets[start_scalar];
    let min_remaining_bytes = 3 * (remaining_segments - 1);
    let mut ends = Vec::new();
    for end_scalar in start_scalar + 1..=max_end {
        let segment_bytes = seed_byte_len(byte_offsets, start_scalar, end_scalar);
        let remaining_bytes = byte_offsets[total_end] - byte_offsets[end_scalar];
        if segment_bytes < 3 || remaining_bytes < min_remaining_bytes {
            continue;
        }
        let balance_distance = segment_bytes
            .saturating_mul(remaining_segments)
            .abs_diff(total_remaining_bytes);
        ends.push((balance_distance, end_scalar));
    }
    ends.sort_unstable();
    ends.into_iter().map(|(_, end_scalar)| end_scalar).collect()
}

fn seed_byte_len(byte_offsets: &[usize], start_scalar: usize, end_scalar: usize) -> usize {
    byte_offsets[end_scalar] - byte_offsets[start_scalar]
}

fn char_count_exceeds(text: &str, limit: usize, deadline: Instant) -> Result<bool, MemoryError> {
    let mut count = 0_usize;
    for _ in text.chars() {
        check_deadline(deadline)?;
        count = count.saturating_add(1);
        if count > limit {
            return Ok(true);
        }
    }
    Ok(false)
}

fn collect_chars_with_deadline(text: &str, deadline: Instant) -> Result<Vec<char>, MemoryError> {
    let mut chars = Vec::new();
    for ch in text.chars() {
        check_deadline(deadline)?;
        chars.push(ch);
    }
    Ok(chars)
}

#[cfg(test)]
fn fuzzy_line_matches(
    line: &str,
    pattern_chars: &[char],
    distance: usize,
    deadline: Instant,
) -> Result<bool, MemoryError> {
    let verifier_seeds = fuzzy_verifier_seeds_from_chars(pattern_chars, distance, deadline)?;
    fuzzy_line_matches_with_seeds(line, pattern_chars, distance, &verifier_seeds, deadline)
}

fn fuzzy_line_matches_with_seeds(
    line: &str,
    pattern_chars: &[char],
    distance: usize,
    verifier_seeds: &[FuzzyVerifierSeed],
    deadline: Instant,
) -> Result<bool, MemoryError> {
    if pattern_chars.is_empty() {
        return Ok(false);
    }

    let line_chars = collect_chars_with_deadline(line, deadline)?;
    check_deadline(deadline)?;
    if line_chars.len().saturating_add(distance) < pattern_chars.len() {
        return Ok(false);
    }

    let starts = fuzzy_candidate_starts(
        &line_chars,
        pattern_chars.len(),
        distance,
        verifier_seeds,
        deadline,
    )?;
    if starts.is_empty() {
        return Ok(false);
    }

    let min_len = pattern_chars.len().saturating_sub(distance);
    let max_len = pattern_chars.len().saturating_add(distance);

    for start in starts {
        check_deadline(deadline)?;
        let remaining = line_chars.len().saturating_sub(start);
        if remaining < min_len {
            continue;
        }
        let max_len = max_len.min(remaining);
        for len in min_len..=max_len {
            check_deadline(deadline)?;
            let end = start + len;
            if bounded_edit_distance(pattern_chars, &line_chars[start..end], distance, deadline)?
                .is_some()
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn fuzzy_candidate_starts(
    line_chars: &[char],
    pattern_len: usize,
    distance: usize,
    verifier_seeds: &[FuzzyVerifierSeed],
    deadline: Instant,
) -> Result<Vec<usize>, MemoryError> {
    let min_len = pattern_len.saturating_sub(distance);
    if min_len > line_chars.len() {
        return Ok(Vec::new());
    }
    let max_start = line_chars.len() - min_len;
    let mut possible_starts = vec![false; max_start + 1];

    for seed in verifier_seeds {
        check_deadline(deadline)?;
        if seed.chars.is_empty() || seed.chars.len() > line_chars.len() {
            continue;
        }

        for occurrence_start in 0..=line_chars.len() - seed.chars.len() {
            check_deadline(deadline)?;
            if line_chars[occurrence_start..occurrence_start + seed.chars.len()] != seed.chars[..] {
                continue;
            }

            let ideal_start = occurrence_start as isize - seed.pattern_start as isize;
            let lower = ideal_start.saturating_sub(distance as isize).max(0) as usize;
            let upper = ideal_start
                .saturating_add(distance as isize)
                .min(max_start as isize) as usize;
            if lower > upper {
                continue;
            }

            for possible in possible_starts.iter_mut().take(upper + 1).skip(lower) {
                check_deadline(deadline)?;
                *possible = true;
            }
        }
    }

    check_deadline(deadline)?;
    Ok(possible_starts
        .into_iter()
        .enumerate()
        .filter_map(|(start, possible)| possible.then_some(start))
        .collect())
}

fn bounded_edit_distance(
    left: &[char],
    right: &[char],
    max_distance: usize,
    deadline: Instant,
) -> Result<Option<usize>, MemoryError> {
    if left.len().abs_diff(right.len()) > max_distance {
        return Ok(None);
    }

    let limit = max_distance.saturating_add(1);
    let mut previous = vec![limit; right.len() + 1];
    let mut current = vec![limit; right.len() + 1];
    for (column, value) in previous
        .iter_mut()
        .enumerate()
        .take(right.len().min(max_distance) + 1)
    {
        *value = column;
    }

    for (left_index, left_char) in left.iter().enumerate() {
        check_deadline(deadline)?;
        let row = left_index + 1;
        let min_column = row.saturating_sub(max_distance).max(1);
        let max_column = row.saturating_add(max_distance).min(right.len());

        current[0] = if row <= max_distance { row } else { limit };
        let mut row_min = current[0];
        if min_column <= max_column && min_column > 1 {
            current[min_column - 1] = limit;
        }

        for column in min_column..=max_column {
            check_deadline(deadline)?;
            let right_char = &right[column - 1];
            let deletion = previous[column].saturating_add(1).min(limit);
            let insertion = current[column - 1].saturating_add(1).min(limit);
            let substitution =
                previous[column - 1].saturating_add(usize::from(left_char != right_char));
            let best = deletion.min(insertion).min(substitution.min(limit));
            current[column] = best;
            row_min = row_min.min(best);
        }
        if max_column < right.len() {
            current[max_column + 1] = limit;
        }

        if row_min > max_distance {
            return Ok(None);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    Ok((previous[right.len()] <= max_distance).then_some(previous[right.len()]))
}

#[cfg(test)]
fn render_line_indexes(
    matched_lines: &BTreeSet<usize>,
    line_count: usize,
    context: usize,
) -> Result<Vec<usize>, MemoryError> {
    render_line_indexes_with_deadline(
        matched_lines,
        line_count,
        context,
        Instant::now() + Duration::from_secs(60),
    )
}

fn render_line_indexes_with_deadline(
    matched_lines: &BTreeSet<usize>,
    line_count: usize,
    context: usize,
    deadline: Instant,
) -> Result<Vec<usize>, MemoryError> {
    let mut lines = Vec::with_capacity(rendered_line_capacity(
        matched_lines.len(),
        line_count,
        context,
    ));
    for_each_render_interval(
        matched_lines,
        line_count,
        context,
        deadline,
        |start, end| {
            for line in start..=end {
                check_deadline(deadline)?;
                lines.push(line);
            }
            Ok(false)
        },
    )?;
    Ok(lines)
}

fn push_rendered_events(
    events: &mut Vec<SearchEvent>,
    doc: &Document,
    matched_lines: &BTreeSet<usize>,
    context: usize,
    max_results: usize,
    deadline: Instant,
) -> Result<bool, MemoryError> {
    let mut matched_iter = matched_lines.iter().copied().peekable();
    for_each_render_interval(
        matched_lines,
        doc.lines.len(),
        context,
        deadline,
        |start, end| {
            push_rendered_interval(
                events,
                doc,
                start,
                end,
                &mut matched_iter,
                max_results,
                deadline,
            )
        },
    )
}

fn for_each_render_interval(
    matched_lines: &BTreeSet<usize>,
    line_count: usize,
    context: usize,
    deadline: Instant,
    mut emit: impl FnMut(usize, usize) -> Result<bool, MemoryError>,
) -> Result<bool, MemoryError> {
    let mut current_interval: Option<(usize, usize)> = None;
    for &match_line in matched_lines {
        check_deadline(deadline)?;
        let Some((start, end)) = render_interval(match_line, line_count, context) else {
            continue;
        };
        match current_interval {
            Some((current_start, current_end)) if start <= current_end.saturating_add(1) => {
                current_interval = Some((current_start, current_end.max(end)));
            }
            Some((current_start, current_end)) => {
                if emit(current_start, current_end)? {
                    return Ok(true);
                }
                current_interval = Some((start, end));
            }
            None => {
                current_interval = Some((start, end));
            }
        }
    }
    if let Some((start, end)) = current_interval
        && emit(start, end)?
    {
        return Ok(true);
    }
    Ok(false)
}

fn render_interval(match_line: usize, line_count: usize, context: usize) -> Option<(usize, usize)> {
    if line_count == 0 {
        return None;
    }
    let start = match_line.saturating_sub(context);
    let end = match_line
        .saturating_add(context)
        .min(line_count.saturating_sub(1));
    Some((start, end))
}

fn rendered_line_capacity(matched_line_count: usize, line_count: usize, context: usize) -> usize {
    let context_width = context.saturating_mul(2).saturating_add(1);
    matched_line_count
        .saturating_mul(context_width)
        .min(line_count)
}

fn push_rendered_interval<I>(
    events: &mut Vec<SearchEvent>,
    doc: &Document,
    start: usize,
    end: usize,
    matched_lines: &mut std::iter::Peekable<I>,
    max_results: usize,
    deadline: Instant,
) -> Result<bool, MemoryError>
where
    I: Iterator<Item = usize>,
{
    while matched_lines
        .peek()
        .is_some_and(|matched_line| *matched_line < start)
    {
        matched_lines.next();
    }

    for line_index in start..=end {
        check_deadline(deadline)?;
        let is_match = matched_lines
            .peek()
            .is_some_and(|matched_line| *matched_line == line_index);
        if is_match {
            matched_lines.next();
        }
        let line_number = (line_index + 1) as u64;
        let text = line_text(doc, line_index);
        check_deadline(deadline)?;
        events.push(SearchEvent::new(
            is_match,
            doc.rendered_path.clone(),
            line_number,
            text,
        ));

        if events.len() >= max_results {
            return Ok(true);
        }
    }

    Ok(false)
}

fn render_search_text_with_deadline(
    events: &[RenderedSearchEvent<'_>],
    deadline: Instant,
) -> Result<String, MemoryError> {
    let mut output = String::with_capacity(render_search_text_capacity_from_rendered(events));
    for (index, event) in events.iter().enumerate() {
        check_deadline(deadline)?;
        if index > 0 {
            output.push('\n');
        }
        event.push_rendered_line(&mut output);
    }
    check_deadline(deadline)?;
    Ok(output)
}

#[cfg(test)]
fn line_ranges(content: &[u8]) -> Vec<LineRange> {
    if content.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut start = 0;
    for index in memchr_iter(b'\n', content) {
        let mut end = index;
        if end > start && content[end - 1] == b'\r' {
            end -= 1;
        }
        ranges.push(LineRange { start, end });
        start = index + 1;
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

fn line_ranges_with_deadline(
    content: &[u8],
    deadline: Instant,
) -> Result<Vec<LineRange>, MemoryError> {
    if content.is_empty() {
        return Ok(Vec::new());
    }

    let mut ranges = Vec::new();
    let mut start = 0;
    for index in memchr_iter(b'\n', content) {
        check_deadline(deadline)?;
        let mut end = index;
        if end > start && content[end - 1] == b'\r' {
            end -= 1;
        }
        ranges.push(LineRange { start, end });
        start = index + 1;
    }
    if start < content.len() {
        check_deadline(deadline)?;
        let mut end = content.len();
        if end > start && content[end - 1] == b'\r' {
            end -= 1;
        }
        ranges.push(LineRange { start, end });
    }
    check_deadline(deadline)?;
    Ok(ranges)
}

fn line_text(doc: &Document, line_index: usize) -> String {
    let range = doc.lines[line_index];
    String::from_utf8_lossy(&doc.content[range.start..range.end]).into_owned()
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    memchr::memmem::find(haystack, needle).is_some()
}

fn contains_subslice_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    if needle.len() == 1 {
        return haystack
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&needle[0]));
    }

    let first_lower = needle[0].to_ascii_lowercase();
    let first_upper = needle[0].to_ascii_uppercase();
    let max_start = haystack.len() - needle.len();
    let mut offset = 0;
    while offset <= max_start {
        let search_space = &haystack[offset..=max_start];
        let relative = if first_lower == first_upper {
            memchr(needle[0], search_space)
        } else {
            memchr2(first_lower, first_upper, search_space)
        };
        let Some(relative) = relative else {
            return false;
        };
        let start = offset + relative;
        if bytes_eq_ignore_ascii_case(&haystack[start..start + needle.len()], needle) {
            return true;
        }
        offset = start + 1;
    }
    false
}

fn contains_word_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }

    let mut offset = 0;
    while offset <= haystack.len() - needle.len() {
        let Some(relative) = memchr::memmem::find(&haystack[offset..], needle) else {
            return false;
        };
        let start = offset + relative;
        if has_ascii_word_boundaries(haystack, start, needle.len()) {
            return true;
        }
        offset = start + 1;
    }
    false
}

fn contains_word_subslice_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    if needle.len() == 1 {
        return haystack.iter().enumerate().any(|(start, candidate)| {
            candidate.eq_ignore_ascii_case(&needle[0])
                && has_ascii_word_boundaries(haystack, start, needle.len())
        });
    }

    let first_lower = needle[0].to_ascii_lowercase();
    let first_upper = needle[0].to_ascii_uppercase();
    let max_start = haystack.len() - needle.len();
    let mut offset = 0;
    while offset <= max_start {
        let search_space = &haystack[offset..=max_start];
        let relative = if first_lower == first_upper {
            memchr(needle[0], search_space)
        } else {
            memchr2(first_lower, first_upper, search_space)
        };
        let Some(relative) = relative else {
            return false;
        };
        let start = offset + relative;
        if bytes_eq_ignore_ascii_case(&haystack[start..start + needle.len()], needle)
            && has_ascii_word_boundaries(haystack, start, needle.len())
        {
            return true;
        }
        offset = start + 1;
    }
    false
}

fn has_ascii_word_boundaries(haystack: &[u8], start: usize, len: usize) -> bool {
    let before_is_boundary = start
        .checked_sub(1)
        .and_then(|index| haystack.get(index))
        .is_none_or(|byte| !is_ascii_word_byte(*byte));
    let after_is_boundary = haystack
        .get(start.saturating_add(len))
        .is_none_or(|byte| !is_ascii_word_byte(*byte));

    before_is_boundary && after_is_boundary
}

fn is_supported_ascii_word_literal(literal: &[u8]) -> bool {
    literal
        .first()
        .is_some_and(|byte| is_ascii_word_byte(*byte))
        && literal.last().is_some_and(|byte| is_ascii_word_byte(*byte))
}

fn is_ascii_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn bytes_eq_ignore_ascii_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn content_contains_nul(content: &[u8]) -> bool {
    memchr(0, content).is_some()
}

fn file_changed_error(message: impl Into<String>, fallback_reason: &'static str) -> MemoryError {
    MemoryError::new("file_changed_during_verification", fallback_reason, message)
}

fn validate_result_file_content_matches(
    doc: &Document,
    metadata: &fs::Metadata,
    deadline: Instant,
) -> Result<(), MemoryError> {
    check_deadline(deadline)?;
    let content = fs::read(&doc.path).map_err(|err| {
        file_changed_error(
            format!("failed to re-read {}: {err}", doc.path.display()),
            "file_changed_during_verification",
        )
    })?;
    check_deadline(deadline)?;
    if metadata.len() != content.len() as u64 || content != doc.content {
        return Err(file_changed_error(
            format!("file changed during memory search: {}", doc.path.display()),
            "file_changed_during_verification",
        ));
    }
    Ok(())
}

fn metadata_stamp_from_metadata(metadata: &fs::Metadata) -> FileStamp {
    FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        change_marker: metadata_change_marker(metadata),
    }
}

fn metadata_stamp_can_validate_without_hash(stamp: &FileStamp) -> bool {
    stamp
        .change_marker
        .as_ref()
        .is_some_and(metadata_change_marker_can_validate_without_hash)
}

#[cfg(all(test, any(unix, windows)))]
fn file_stamp_from_parts(metadata: &fs::Metadata) -> FileStamp {
    FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        change_marker: metadata_change_marker(metadata),
    }
}

fn file_stamp_from_parts_with_deadline(
    metadata: &fs::Metadata,
    deadline: Instant,
) -> Result<FileStamp, MemoryError> {
    check_deadline(deadline)?;
    Ok(FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        change_marker: metadata_change_marker(metadata),
    })
}

fn file_metadata_matches_without_hash(stamp: &FileStamp, metadata: &fs::Metadata) -> bool {
    stamp.len == metadata.len()
        && stamp.modified == metadata.modified().ok()
        && metadata_stamp_can_validate_without_hash(stamp)
        && stamp.change_marker == metadata_change_marker(metadata)
}

fn metadata_stamp_matches(stamp: &FileStamp, metadata: &fs::Metadata) -> bool {
    stamp.len == metadata.len()
        && stamp.modified == metadata.modified().ok()
        && stamp.change_marker == metadata_change_marker(metadata)
}

#[cfg(unix)]
fn metadata_change_marker_can_validate_without_hash(_marker: &MetadataChangeMarker) -> bool {
    true
}

#[cfg(not(unix))]
fn metadata_change_marker_can_validate_without_hash(_marker: &MetadataChangeMarker) -> bool {
    false
}

#[cfg(unix)]
fn metadata_change_marker(metadata: &fs::Metadata) -> Option<MetadataChangeMarker> {
    use std::os::unix::fs::MetadataExt;

    Some(UnixMetadataChangeMarker {
        dev: metadata.dev(),
        ino: metadata.ino(),
        mode: metadata.mode(),
        ctime: metadata.ctime(),
        ctime_nsec: metadata.ctime_nsec(),
    })
}

#[cfg(windows)]
fn metadata_change_marker(metadata: &fs::Metadata) -> Option<MetadataChangeMarker> {
    use std::os::windows::fs::MetadataExt;

    // Stable Rust exposes Windows creation, write-time, and attribute metadata,
    // but not file_index/change_time. Use the stable fields as a no-dependency
    // identity/change marker and keep byte comparison as the authoritative guard.
    Some(WindowsMetadataChangeMarker {
        creation_time: metadata.creation_time(),
        last_write_time: metadata.last_write_time(),
        file_attributes: metadata.file_attributes(),
    })
}

#[cfg(not(any(unix, windows)))]
fn metadata_change_marker(_metadata: &fs::Metadata) -> Option<MetadataChangeMarker> {
    None
}

fn check_deadline(deadline: Instant) -> Result<(), MemoryError> {
    if Instant::now() >= deadline {
        Err(MemoryError::timeout())
    } else {
        Ok(())
    }
}

/// Best-effort cancellation observer for hot loops. The token is captured once by
/// the caller (via `current_cancellation_token()`) and re-checked once per iteration.
/// Each call is a single atomic load when the token is present.
#[inline]
fn check_cancellation(
    token: Option<&tokio_util::sync::CancellationToken>,
) -> Result<(), MemoryError> {
    if let Some(token) = token
        && token.is_cancelled()
    {
        return Err(MemoryError::cancelled());
    }
    Ok(())
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

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        })
        .unwrap_or(default)
}

fn force_full_scope_on_ignore_enabled() -> bool {
    env_bool("TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE", false)
}

fn warm_cache_globs_from_env() -> Vec<String> {
    let raw = std::env::var("TOOLS_SEARCH_INDEX_WARM_GLOBS")
        .unwrap_or_else(|_| DEFAULT_WARM_CACHE_GLOBS.to_string());
    warm_cache_globs_from_raw(&raw)
}

fn warm_cache_globs_from_raw(raw: &str) -> Vec<String> {
    let mut globs = Vec::new();
    for glob in raw.split([',', ';']).map(str::trim) {
        if !glob.is_empty()
            && !glob.eq_ignore_ascii_case("none")
            && !globs.iter().any(|existing| existing == glob)
        {
            globs.push(glob.to_string());
        }
    }
    globs
}

fn index_cache_max_entries() -> usize {
    env_usize(
        "TOOLS_SEARCH_INDEX_CACHE_MAX_ENTRIES",
        DEFAULT_INDEX_CACHE_MAX_ENTRIES,
    )
    .max(1)
}

fn index_cache_max_bytes() -> Option<u64> {
    match env_u64(
        "TOOLS_SEARCH_INDEX_CACHE_MAX_BYTES",
        DEFAULT_INDEX_CACHE_MAX_BYTES,
    ) {
        0 => None,
        max_bytes => Some(max_bytes),
    }
}

fn index_cache_limits() -> IndexCacheLimits {
    IndexCacheLimits {
        max_entries: index_cache_max_entries(),
        max_bytes: index_cache_max_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[derive(Clone, Default)]
    struct DedupeProbeState {
        first_build_entered: bool,
        waiter_entered: bool,
        build_count: usize,
        release_first_build: bool,
    }

    type DedupeProbe = Arc<(Mutex<DedupeProbeState>, Condvar)>;

    static INDEX_BUILD_HOOK_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static FORCE_FULL_SCOPE_ENV_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct IndexBuildHookGuard {
        _lock: MutexGuard<'static, ()>,
        previous_build: Option<Arc<IndexBuildTestHook>>,
        previous_wait: Option<Arc<IndexBuildWaitTestHook>>,
    }

    struct ForceFullScopeEnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Option<OsString>,
    }

    impl Drop for ForceFullScopeEnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => unsafe {
                    std::env::set_var("TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE", value);
                },
                None => unsafe {
                    std::env::remove_var("TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE");
                },
            }
        }
    }

    impl Drop for IndexBuildHookGuard {
        fn drop(&mut self) {
            replace_index_build_test_hook(self.previous_build.take());
            replace_index_build_wait_test_hook(self.previous_wait.take());
        }
    }

    fn install_index_build_hooks(
        build_hook: Option<Arc<IndexBuildTestHook>>,
        wait_hook: Option<Arc<IndexBuildWaitTestHook>>,
    ) -> IndexBuildHookGuard {
        let lock = INDEX_BUILD_HOOK_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        IndexBuildHookGuard {
            _lock: lock,
            previous_build: replace_index_build_test_hook(build_hook),
            previous_wait: replace_index_build_wait_test_hook(wait_hook),
        }
    }

    fn force_full_scope_env(value: Option<&str>) -> ForceFullScopeEnvGuard {
        let lock = FORCE_FULL_SCOPE_ENV_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE");
        match value {
            Some(value) => unsafe {
                std::env::set_var("TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE", value);
            },
            None => unsafe {
                std::env::remove_var("TOOLS_SEARCH_FORCE_FULL_SCOPE_ON_IGNORE");
            },
        }
        ForceFullScopeEnvGuard {
            _lock: lock,
            previous,
        }
    }

    fn wait_for_probe(
        probe: &DedupeProbe,
        timeout: Duration,
        description: &str,
        condition: impl Fn(&DedupeProbeState) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        let (lock, cvar) = &**probe;
        let mut state = lock.lock().expect("probe mutex");
        loop {
            if condition(&state) {
                return;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "timed out waiting for {description}; build_count={}, waiter_entered={}",
                state.build_count,
                state.waiter_entered
            );
            let (next_state, result) = cvar
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .expect("probe condvar");
            state = next_state;
            if result.timed_out() {
                assert!(
                    condition(&state),
                    "timed out waiting for {description}; build_count={}, waiter_entered={}",
                    state.build_count,
                    state.waiter_entered
                );
                return;
            }
        }
    }

    fn probe_state(probe: &DedupeProbe) -> DedupeProbeState {
        let (lock, _) = &**probe;
        lock.lock().expect("probe mutex").clone()
    }

    fn release_first_build(probe: &DedupeProbe) {
        let (lock, cvar) = &**probe;
        let mut state = lock.lock().expect("probe mutex");
        state.release_first_build = true;
        cvar.notify_all();
    }

    fn install_dedupe_probe_hooks(
        hook_root: String,
        probe: &DedupeProbe,
        build_count: &Arc<AtomicUsize>,
    ) -> IndexBuildHookGuard {
        let build_probe = probe.clone();
        let build_count_for_hook = build_count.clone();
        let build_hook_root = hook_root.clone();
        let build_hook: Arc<IndexBuildTestHook> = Arc::new(move |selector, _generation| {
            if selector.root_arg() != build_hook_root {
                return;
            }

            let current_count = build_count_for_hook.fetch_add(1, Ordering::SeqCst) + 1;
            let (lock, cvar) = &*build_probe;
            let mut state = lock.lock().expect("probe mutex");
            state.build_count = current_count;
            if current_count == 1 {
                state.first_build_entered = true;
                cvar.notify_all();
                while !state.release_first_build {
                    state = cvar.wait(state).expect("probe condvar");
                }
            } else {
                cvar.notify_all();
            }
        });

        let wait_probe = probe.clone();
        let wait_hook: Arc<IndexBuildWaitTestHook> = Arc::new(move |key| {
            if key.root != hook_root {
                return;
            }

            let (lock, cvar) = &*wait_probe;
            let mut state = lock.lock().expect("probe mutex");
            state.waiter_entered = true;
            cvar.notify_all();
        });

        install_index_build_hooks(Some(build_hook), Some(wait_hook))
    }

    #[test]
    fn fixed_string_trigram_extraction_deduplicates_in_order() {
        assert_eq!(
            literal_trigrams(b"ababa"),
            vec![[b'a', b'b', b'a'], [b'b', b'a', b'b']]
        );
    }

    #[test]
    fn fused_trigram_extraction_matches_legacy_postings() {
        let fixtures: &[&[u8]] = &[
            b"",
            b"ab",
            b"abc",
            b"ababa",
            b"Needle NEEDLE needle",
            "prefix café NEEDLE suffix".as_bytes(),
            b"edge-start needle",
            b"needle edge-end",
        ];

        for (index, content) in fixtures.iter().enumerate() {
            let doc_id = DocId::from_index(index).expect("doc id");
            let mut postings = HashMap::new();
            let mut folded_postings = HashMap::new();
            index_document_trigrams_with_deadline(
                doc_id,
                content,
                &mut postings,
                &mut folded_postings,
                Instant::now() + Duration::from_secs(30),
            )
            .expect("fused trigrams");

            let (legacy_postings, legacy_folded_postings) =
                legacy_document_trigrams(doc_id, content);
            assert_eq!(postings, legacy_postings, "sensitive fixture {index}");
            assert_eq!(
                folded_postings, legacy_folded_postings,
                "folded fixture {index}"
            );
        }
    }

    #[test]
    fn candidate_intersection_uses_all_postings() {
        let first = postings_for_test(&[1, 2, 4, 7]);
        let second = postings_for_test(&[2, 3, 4, 8]);
        let third = postings_for_test(&[0, 2, 4, 9]);
        assert_eq!(
            intersect_postings(
                &[&first, &second, &third],
                Instant::now() + Duration::from_secs(30)
            )
            .expect("intersect postings"),
            doc_ids(&[2, 4])
        );
    }

    #[test]
    fn postings_track_document_frequency_and_sorted_unique_doc_ids() {
        let postings = postings_for_test(&[3, 1, 3, 2]);

        assert_eq!(postings.document_frequency().get(), 3);
        assert_eq!(postings.to_vec(), doc_ids(&[1, 2, 3]));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn doc_id_rejects_indices_outside_u32_width() {
        let err = DocId::from_index(u32::MAX as usize + 1).expect_err("wide doc id should fail");

        assert_eq!(err.error_type, "resource_limit_exceeded");
        assert_eq!(err.fallback_reason, "doc_id_width_exceeded");
    }

    #[test]
    fn candidate_expression_builders_flatten_and_dedupe_children() {
        let repeated = CandidateExpr::Seed(b"needle".to_vec());
        let other = CandidateExpr::Seed(b"haystack".to_vec());

        let expr = candidate_and([
            repeated.clone(),
            CandidateExpr::And(vec![other.clone(), repeated.clone()]),
        ])
        .expect("deduped expression");

        assert_eq!(expr, CandidateExpr::And(vec![repeated, other]));
    }

    #[test]
    fn candidate_selectivity_order_uses_deterministic_tie_breakers() {
        let snapshot = snapshot_with_postings(vec![
            (*b"aaa", vec![0]),
            (*b"bbb", vec![1]),
            (*b"ccc", vec![2, 3]),
        ]);
        let and_expr = CandidateExpr::And(vec![
            CandidateExpr::Seed(b"bbb".to_vec()),
            CandidateExpr::Seed(b"aaa".to_vec()),
        ]);
        let children = vec![
            CandidateExpr::Seed(b"ccc".to_vec()),
            and_expr.clone(),
            CandidateExpr::Seed(b"bbb".to_vec()),
            CandidateExpr::Seed(b"aaa".to_vec()),
        ];

        let ordered = candidate_children_by_selectivity(
            &snapshot,
            &children,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("ordered children")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();

        assert_eq!(
            ordered,
            vec![
                CandidateExpr::Seed(b"aaa".to_vec()),
                CandidateExpr::Seed(b"bbb".to_vec()),
                and_expr,
                CandidateExpr::Seed(b"ccc".to_vec()),
            ]
        );
    }

    #[test]
    fn fuzzy_seed_selectivity_order_uses_deterministic_tie_breakers() {
        let snapshot = snapshot_with_postings(vec![(*b"aaa", vec![0]), (*b"bbb", vec![1])]);
        let seeds = vec![b"bbb".to_vec(), b"aaaa".to_vec(), b"aaa".to_vec()];

        let ordered = candidate_seeds_by_selectivity(
            &snapshot,
            &seeds,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("ordered seeds")
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();

        assert_eq!(
            ordered,
            vec![b"aaaa".to_vec(), b"aaa".to_vec(), b"bbb".to_vec()]
        );
    }

    #[test]
    fn regex_candidate_extraction_does_not_consume_candidate_doc_budget() {
        let mut req = memory_req(Path::new("."));
        req.pattern = "nee.*dle".to_string();
        req.fixed_strings = Some(false);
        let req = req.normalize();
        let mut limits = test_limits();
        limits.max_candidates = 0;

        let plan = eligible_query_plan_with_limits(
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("regex plan should not depend on candidate doc budget");

        assert!(matches!(plan, QueryPlan::Regex { .. }));
    }

    #[test]
    fn single_trigram_candidate_limit_falls_back_before_phase_two() {
        let snapshot = snapshot_with_postings(vec![(*b"aaa", vec![0, 1])]);
        let plan = QueryPlan::Exact {
            literal: b"aaa".to_vec(),
            case: LiteralCase::Sensitive,
        };

        let err = candidates_for_plan(
            &snapshot,
            &plan,
            &Limits {
                max_candidates: 1,
                ..test_limits()
            },
            Instant::now() + Duration::from_secs(30),
        )
        .expect_err("single posting should exceed candidate limit");

        assert_eq!(err.error_type, "resource_limit_exceeded");
        assert_eq!(err.fallback_reason, "max_candidates_exceeded");
        assert!(err.fallback_allowed);
    }

    #[test]
    fn multi_trigram_candidate_limit_uses_exact_intersection_before_fallback() {
        let snapshot =
            snapshot_with_postings(vec![(*b"abc", vec![0, 2, 4]), (*b"bcd", vec![1, 2, 3])]);
        let plan = QueryPlan::Exact {
            literal: b"abcd".to_vec(),
            case: LiteralCase::Sensitive,
        };

        let candidates = candidates_for_plan(
            &snapshot,
            &plan,
            &Limits {
                max_candidates: 1,
                ..test_limits()
            },
            Instant::now() + Duration::from_secs(30),
        )
        .expect("intersection should fit candidate limit");

        assert_eq!(candidates, doc_ids(&[2]));
    }

    #[test]
    fn short_literal_direct_scan_uses_candidate_and_line_limits() {
        let mut snapshot = empty_snapshot(1);
        snapshot.documents = vec![
            document_for_test("first.txt", b"a\nmiss\n"),
            document_for_test("second.txt", b"miss\na\n"),
        ];

        let plan = QueryPlan::ShortExact {
            literal: b"a".to_vec(),
            case: LiteralCase::Sensitive,
        };

        let err = candidates_for_plan(
            &snapshot,
            &plan,
            &Limits {
                max_candidates: 1,
                ..test_limits()
            },
            Instant::now() + Duration::from_secs(30),
        )
        .expect_err("short literal scan should enforce candidate limit");
        assert_eq!(err.error_type, "resource_limit_exceeded");
        assert_eq!(err.fallback_reason, "max_candidates_exceeded");

        let err = candidates_for_plan(
            &snapshot,
            &plan,
            &Limits {
                max_short_literal_scan_lines: 1,
                ..test_limits()
            },
            Instant::now() + Duration::from_secs(30),
        )
        .expect_err("short literal scan should enforce line limit");
        assert_eq!(err.error_type, "resource_limit_exceeded");
        assert_eq!(err.fallback_reason, "max_short_literal_scan_lines_exceeded");
    }

    #[test]
    fn empty_and_child_short_circuits_over_budget_sibling() {
        let snapshot = snapshot_with_postings(vec![(*b"hot", vec![0, 1, 2])]);
        let expr = CandidateExpr::And(vec![
            CandidateExpr::Seed(b"hot".to_vec()),
            CandidateExpr::Seed(b"abs".to_vec()),
        ]);

        let candidates = candidates_for_candidate_expr(
            &snapshot,
            &expr,
            0,
            Instant::now() + Duration::from_secs(30),
        )
        .expect("empty child should short-circuit before over-budget sibling");

        assert!(candidates.is_empty());
    }

    #[test]
    fn repeated_candidate_estimates_are_memoized_before_negative_short_circuit() {
        let snapshot = snapshot_with_postings(vec![(*b"hot", vec![0, 1, 2])]);
        let repeated = CandidateExpr::And(vec![
            CandidateExpr::Seed(b"hot".to_vec()),
            CandidateExpr::Seed(b"abs".to_vec()),
        ]);
        let expr = CandidateExpr::Or(vec![repeated.clone(), repeated]);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut estimate_cache = CandidateEstimateCache::default();

        let estimate = candidate_expr_estimated_docs_with_cache(
            &snapshot,
            &expr,
            deadline,
            &mut estimate_cache,
        )
        .expect("estimate");
        assert_eq!(estimate, 0);
        assert_eq!(estimate_cache.literal_estimates.len(), 2);

        let candidates = candidates_for_candidate_expr_with_cache(
            &snapshot,
            &expr,
            0,
            deadline,
            &mut estimate_cache,
        )
        .expect("zero estimate should avoid over-budget sibling candidates");
        assert!(candidates.is_empty());
    }

    #[test]
    fn contains_subslice_checks_seeded_candidates_without_missing_overlap() {
        assert!(contains_subslice(b"aaaab", b"aaab"));
        assert!(contains_subslice(b"prefix needle suffix", b"needle"));
        assert!(!contains_subslice(b"short", b"needle"));
        assert!(!contains_subslice(b"anything", b""));
    }

    #[test]
    fn ascii_case_insensitive_subslice_checks_seeded_candidates() {
        assert!(contains_subslice_ascii_case_insensitive(
            b"prefix NEEDLE suffix",
            b"needle"
        ));
        assert!(contains_subslice_ascii_case_insensitive(
            b"prefix nEeDlE suffix",
            b"Needle"
        ));
        assert!(contains_subslice_ascii_case_insensitive(b"ABC", b"a"));
        assert!(!contains_subslice_ascii_case_insensitive(
            b"haystack",
            b"needle"
        ));
        assert!(!contains_subslice_ascii_case_insensitive(b"anything", b""));
    }

    #[test]
    fn word_subslice_checks_ascii_boundaries() {
        assert!(contains_word_subslice(b"foo", b"foo"));
        assert!(contains_word_subslice(b"foo-bar", b"foo"));
        assert!(contains_word_subslice(b"(foo)", b"foo"));
        assert!(contains_word_subslice(b"bar foo", b"foo"));
        assert!(contains_word_subslice(b"foo bar", b"foo"));
        assert!(!contains_word_subslice(b"foo_bar", b"foo"));
        assert!(!contains_word_subslice(b"foo1", b"foo"));
        assert!(!contains_word_subslice(b"1foo", b"foo"));
        assert!(!contains_word_subslice(b"barfoo", b"foo"));

        assert!(contains_word_subslice_ascii_case_insensitive(
            b"prefix FOO suffix",
            b"foo"
        ));
        assert!(!contains_word_subslice_ascii_case_insensitive(
            b"prefix FOO_bar suffix",
            b"foo"
        ));
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
            },
            content: b"alpha\nneedle one\nmiddle\nneedle two\nomega\n".to_vec(),
            lines: line_ranges(b"alpha\nneedle one\nmiddle\nneedle two\nomega\n"),
        };
        let mut verification_stats = VerificationStats::default();
        let matched = matching_line_indexes(
            &doc,
            &QueryPlan::Exact {
                literal: b"needle".to_vec(),
                case: LiteralCase::Sensitive,
            },
            None,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            &mut verification_stats,
        )
        .expect("match lines");
        assert_eq!(
            render_line_indexes(&matched, doc.lines.len(), 1).expect("render lines"),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn verify_and_render_preserves_context_order_and_truncation() {
        let content = b"alpha\nneedle one\nmiddle\nneedle two\nomega\n";
        let snapshot = IndexSnapshot {
            generation: 1,
            documents: vec![Document {
                path: PathBuf::from("sample.txt"),
                rendered_path: "sample.txt".to_string(),
                stamp: FileStamp {
                    len: 0,
                    modified: None,
                    change_marker: None,
                },
                content: content.to_vec(),
                lines: line_ranges(content),
            }],
            scope_fingerprint: ScopeFingerprint::default(),
            ignore_fingerprint: None,
            postings: PostingsIndex::default(),
            ascii_folded_postings: PostingsIndex::default(),
            indexed_bytes: content.len() as u64,
            all_content_utf8: true,
        };
        let req = SearchRequest {
            pattern: "needle".to_string(),
            path: Some("sample.txt".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: Some(1),
            max_results: Some(4),
            timeout_ms: None,
            fuzzy: None,
        }
        .normalize();

        let candidates = doc_ids(&[0]);
        let (events, truncated, _, result_doc_ids) = verify_and_render(
            &snapshot,
            &candidates,
            &QueryPlan::Exact {
                literal: b"needle".to_vec(),
                case: LiteralCase::Sensitive,
            },
            None,
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("verify and render");

        assert!(truncated);
        assert_eq!(result_doc_ids, BTreeSet::from([DocId(0)]));
        assert_eq!(events.len(), 4);
        assert_eq!(
            events
                .iter()
                .map(|event| (event.is_match, event.line_number, event.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (false, 1, "alpha"),
                (true, 2, "needle one"),
                (false, 3, "middle"),
                (true, 4, "needle two"),
            ]
        );
    }

    #[test]
    fn streaming_verification_stops_inside_doc_when_budget_reached() {
        let content = (0..100)
            .map(|index| format!("needle {index}\n"))
            .collect::<String>()
            .into_bytes();
        let mut snapshot = empty_snapshot(1);
        snapshot.documents = vec![Document {
            path: PathBuf::from("dense.txt"),
            rendered_path: "dense.txt".to_string(),
            stamp: FileStamp {
                len: content.len() as u64,
                modified: None,
                change_marker: None,
            },
            content: content.clone(),
            lines: line_ranges(&content),
        }];
        let req = SearchRequest {
            pattern: "needle".to_string(),
            path: Some("dense.txt".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: Some(0),
            max_results: Some(5),
            timeout_ms: None,
            fuzzy: None,
        }
        .normalize();

        let (events, truncated, verification_stats, _) = verify_and_render(
            &snapshot,
            &doc_ids(&[0]),
            &QueryPlan::Exact {
                literal: b"needle".to_vec(),
                case: LiteralCase::Sensitive,
            },
            None,
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("streaming verification");

        assert!(truncated);
        assert_eq!(events.len(), 5);
        assert!(
            verification_stats.verified_lines < snapshot.documents[0].lines.len(),
            "verification should stop before scanning the whole dense file"
        );
    }

    #[test]
    fn streaming_verification_preserves_context_after_last_emitted_match() {
        let content = b"zero\nneedle one\nneedle two\nthree\nfour\n".to_vec();
        let mut snapshot = empty_snapshot(1);
        snapshot.documents = vec![Document {
            path: PathBuf::from("context.txt"),
            rendered_path: "context.txt".to_string(),
            stamp: FileStamp {
                len: content.len() as u64,
                modified: None,
                change_marker: None,
            },
            content: content.clone(),
            lines: line_ranges(&content),
        }];
        let req = SearchRequest {
            pattern: "needle".to_string(),
            path: Some("context.txt".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: None,
            glob: None,
            hidden: None,
            follow: None,
            no_ignore: None,
            context: Some(1),
            max_results: Some(4),
            timeout_ms: None,
            fuzzy: None,
        }
        .normalize();

        let (events, truncated, _, _) = verify_and_render(
            &snapshot,
            &doc_ids(&[0]),
            &QueryPlan::Exact {
                literal: b"needle".to_vec(),
                case: LiteralCase::Sensitive,
            },
            None,
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("streaming verification");

        assert!(truncated);
        assert_eq!(
            events
                .iter()
                .map(|event| (event.is_match, event.line_number, event.text.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (false, 1, "zero"),
                (true, 2, "needle one"),
                (true, 3, "needle two"),
                (false, 4, "three"),
            ]
        );
    }

    #[test]
    fn line_text_uses_lossy_utf8_rendering() {
        let doc = Document {
            path: PathBuf::from("sample.txt"),
            rendered_path: "sample.txt".to_string(),
            stamp: FileStamp {
                len: 0,
                modified: None,
                change_marker: None,
            },
            content: vec![b'f', 0x80, b'o', b'\n'],
            lines: line_ranges(&[b'f', 0x80, b'o', b'\n']),
        };

        assert_eq!(line_text(&doc, 0), "f\u{fffd}o");
    }

    #[test]
    fn line_ranges_strip_crlf_and_final_cr() {
        let content = b"alpha\r\nneedle\r\nomega\r";
        let ranges = line_ranges(content);
        let rendered = ranges
            .iter()
            .map(|range| String::from_utf8_lossy(&content[range.start..range.end]).into_owned())
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["alpha", "needle", "omega"]);
    }

    #[test]
    fn expired_deadline_stops_candidate_retrieval_and_set_operations() {
        let snapshot = empty_snapshot(1);
        let plan = QueryPlan::Exact {
            literal: b"needle".to_vec(),
            case: LiteralCase::Sensitive,
        };

        assert_timeout(
            candidates_for_plan(&snapshot, &plan, &test_limits(), Instant::now())
                .expect_err("candidate retrieval should time out"),
        );
        assert_timeout(
            union_postings(
                doc_ids(&[1]),
                doc_ids(&[2]),
                DEFAULT_MAX_CANDIDATES,
                Instant::now(),
            )
            .expect_err("union should time out"),
        );

        let first = postings_for_test(&[1, 2, 3]);
        let second = postings_for_test(&[2, 3, 4]);
        assert_timeout(
            intersect_postings(&[&first, &second], Instant::now())
                .expect_err("postings intersection should time out"),
        );
    }

    #[test]
    fn expired_deadline_stops_index_build_and_freshness_check() {
        let root = workspace_test_dir("expired_deadline_stops_index_build_and_freshness_check");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");

        let req = memory_req(&root).normalize();
        let limits = test_limits();
        assert_timeout(
            build_index(&req, &limits, Instant::now(), false, 1)
                .expect_err("index build should time out"),
        );

        let snapshot = build_index(
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
            false,
            2,
        )
        .expect("index");
        assert_timeout(
            check_snapshot_fresh(&req, &snapshot, Instant::now())
                .expect_err("freshness check should time out"),
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expired_deadline_stops_rendering_and_fuzzy_verification() {
        let root = workspace_test_dir("expired_deadline_stops_rendering_and_fuzzy");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");

        assert_timeout(
            literal_trigrams_with_deadline(b"needle", Instant::now())
                .expect_err("trigram extraction should time out"),
        );

        assert_timeout(
            render_line_indexes_with_deadline(&BTreeSet::from([0_usize]), 1, 0, Instant::now())
                .expect_err("rendering should time out"),
        );

        let pattern: Vec<char> = "needle".chars().collect();
        assert_timeout(
            fuzzy_line_matches("haystack needle", &pattern, 1, Instant::now())
                .expect_err("fuzzy verification should time out"),
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn memory_error_payload_is_structured() {
        let req = SearchRequest {
            pattern: "ab".to_string(),
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
        let error = eligible_query_plan(&req).expect_err("short literal should be ineligible");
        let req = req.normalize();
        let outcome = error.into_tool_outcome(&req);
        assert_eq!(outcome.0["isError"], true);
        assert_eq!(outcome.0["backend"], "memory");
        assert_eq!(outcome.0["error_type"], "unsupported_search_option");
        assert_eq!(
            outcome.0["fallback_reason"],
            "query_without_required_trigram"
        );
        assert_eq!(outcome.0["memory_eligibility"], "error");
        assert_eq!(outcome.0["exit_code"], Value::Null);
        assert_eq!(outcome.0["timed_out"], false);
    }

    fn regex_req(pattern: &str, case: Option<&str>) -> NormalizedSearchRequest {
        SearchRequest {
            pattern: pattern.to_string(),
            path: Some(".".to_string()),
            case: case.map(ToOwned::to_owned),
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
        }
        .normalize()
    }

    fn regex_classifier_fallback(pattern: &str, case: Option<&str>) -> RegexDialectFallback {
        match classify_regex_dialect_for_planning_inner(
            &regex_req(pattern, case),
            Instant::now() + Duration::from_secs(30),
        )
        .expect_err("regex should be classified as fallback")
        {
            RegexDialectClassificationError::Fallback(fallback) => fallback,
            RegexDialectClassificationError::Timeout(err) => {
                panic!("unexpected timeout classification: {}", err.message)
            }
        }
    }

    #[test]
    fn regex_dialect_classifier_accepts_supported_sensitive_line_regex() {
        let plan = classify_regex_dialect_for_planning_inner(
            &regex_req("needle.*haystack", Some("sensitive")),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("supported seeded regex dialect");

        assert_eq!(plan.decision, RegexDialectDecision::eligible());
        assert!(!hir_can_match_lf(&plan.hir));
    }

    #[test]
    fn regex_dialect_classifier_reports_case_and_surface_fallbacks() {
        let cases = [
            (
                "case-insensitive",
                "needle.*haystack",
                Some("insensitive"),
                MemoryRegexVerifierBehavior::RequiresUnsupportedCaseFolding,
                UgrepRegexBehavior::CaseInsensitiveLineOriented,
                RegexFallbackReason::CaseInsensitive,
            ),
            (
                "smart-case-insensitive",
                "needle.*haystack",
                None,
                MemoryRegexVerifierBehavior::RequiresUnsupportedSmartCaseFolding,
                UgrepRegexBehavior::SmartCaseInsensitiveLineOriented,
                RegexFallbackReason::SmartCaseInsensitive,
            ),
            (
                "inline-construct",
                "(?i)needle",
                Some("sensitive"),
                MemoryRegexVerifierBehavior::UnsupportedInlineConstruct,
                UgrepRegexBehavior::DelegatedBackendDialect,
                RegexFallbackReason::Backend,
            ),
            (
                "line-break-escape",
                "needle\\Rhaystack",
                Some("sensitive"),
                MemoryRegexVerifierBehavior::MayConsumeLineTerminator,
                UgrepRegexBehavior::DelegatedLineBreakDialect,
                RegexFallbackReason::Multiline,
            ),
            (
                "literal-line-break",
                "needle\nhaystack",
                Some("sensitive"),
                MemoryRegexVerifierBehavior::MayConsumeLineTerminator,
                UgrepRegexBehavior::DelegatedLineBreakDialect,
                RegexFallbackReason::Multiline,
            ),
            (
                "parser-rejected",
                "needle(",
                Some("sensitive"),
                MemoryRegexVerifierBehavior::ParserRejectedPattern,
                UgrepRegexBehavior::DelegatedBackendDialect,
                RegexFallbackReason::Backend,
            ),
        ];

        for (name, pattern, case, expected_memory, expected_ugrep, expected_reason) in cases {
            let fallback = regex_classifier_fallback(pattern, case);
            assert_eq!(
                fallback.decision.memory_verifier, expected_memory,
                "{name}: memory verifier classification mismatch"
            );
            assert_eq!(
                fallback.decision.ugrep_behavior, expected_ugrep,
                "{name}: ugrep behavior classification mismatch"
            );
            assert_eq!(
                fallback.decision.fallback_reason,
                Some(expected_reason),
                "{name}: fallback reason classification mismatch"
            );
            let err = fallback.into_memory_error();
            assert_eq!(err.error_type, expected_reason.error_type(), "{name}");
            assert_eq!(err.fallback_reason, expected_reason.as_str(), "{name}");
        }
    }

    #[test]
    fn regex_dialect_classifier_reports_hir_and_verifier_fallbacks() {
        let fallback = regex_classifier_fallback("needle\\D+haystack", Some("sensitive"));
        assert_eq!(
            fallback.decision.memory_verifier,
            MemoryRegexVerifierBehavior::MayConsumeLineTerminator
        );
        assert_eq!(
            fallback.decision.ugrep_behavior,
            UgrepRegexBehavior::DelegatedLineBreakDialect
        );
        assert_eq!(
            fallback.decision.fallback_reason,
            Some(RegexFallbackReason::Multiline)
        );

        let mut limits = test_limits();
        limits.regex_size_limit_bytes = 1;
        let fallback = build_classified_regex_matcher("needle.*haystack", &limits)
            .expect_err("regex verifier size limit should reject the pattern");
        assert_eq!(
            fallback.decision.memory_verifier,
            MemoryRegexVerifierBehavior::VerifierRejectedPattern
        );
        assert_eq!(
            fallback.decision.ugrep_behavior,
            UgrepRegexBehavior::DelegatedBackendDialect
        );
        assert_eq!(
            fallback.decision.fallback_reason,
            Some(RegexFallbackReason::Backend)
        );
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
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected regex plan"),
        }
    }

    #[test]
    fn repeated_required_regex_literals_are_deduped() {
        let req = SearchRequest {
            pattern: "needle.*needle".to_string(),
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
                assert_eq!(candidates, CandidateExpr::Seed(b"needle".to_vec()));
            }
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected regex plan"),
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
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected regex plan"),
        }
    }

    #[test]
    fn seeded_regex_bounded_repetition_builds_seed_from_short_literals() {
        let req = SearchRequest {
            pattern: "(ab){2}".to_string(),
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
        let plan = eligible_query_plan(&req).expect("bounded repeated literal should be eligible");
        match plan {
            QueryPlan::Regex { candidates, .. } => {
                assert_eq!(candidates, CandidateExpr::Seed(b"abab".to_vec()));
            }
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected regex plan"),
        }
    }

    #[test]
    fn seeded_regex_short_branch_alternation_unions_exact_literals() {
        let req = SearchRequest {
            pattern: "(ab|cd){2}".to_string(),
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
        let plan = eligible_query_plan(&req).expect("bounded short alternation should be eligible");
        match plan {
            QueryPlan::Regex { candidates, .. } => {
                assert_eq!(
                    candidates,
                    CandidateExpr::Or(vec![
                        CandidateExpr::Seed(b"abab".to_vec()),
                        CandidateExpr::Seed(b"abcd".to_vec()),
                        CandidateExpr::Seed(b"cdab".to_vec()),
                        CandidateExpr::Seed(b"cdcd".to_vec())
                    ])
                );
            }
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected regex plan"),
        }
    }

    #[test]
    fn seeded_regex_optional_short_concat_uses_safe_or_literals() {
        let req = SearchRequest {
            pattern: "ab?cd".to_string(),
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
        let plan = eligible_query_plan(&req).expect("optional short concat should be eligible");
        match plan {
            QueryPlan::Regex { candidates, .. } => {
                assert_eq!(
                    candidates,
                    CandidateExpr::Or(vec![
                        CandidateExpr::Seed(b"acd".to_vec()),
                        CandidateExpr::Seed(b"abcd".to_vec())
                    ])
                );
            }
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected regex plan"),
        }
    }

    #[test]
    fn unseeded_unbounded_short_repetition_still_falls_back() {
        let req = SearchRequest {
            pattern: "ab+c".to_string(),
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
        let error = eligible_query_plan(&req).expect_err("unbounded short regex should fall back");
        assert_eq!(error.error_type, "unsupported_regex_dialect");
        assert_eq!(error.fallback_reason, "query_without_required_trigram");
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
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected regex plan"),
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
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected regex plan"),
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

        req.pattern = "café".to_string();
        req.fixed_strings = Some(true);
        let error = eligible_query_plan(&req).expect_err("non-ASCII word literal should fall back");
        assert_eq!(error.error_type, "unsupported_search_option");
        assert_eq!(error.fallback_reason, "unsupported_word_regexp");

        req.pattern = "fo".to_string();
        let error = eligible_query_plan(&req).expect_err("short word literal should fall back");
        assert_eq!(error.error_type, "unsupported_search_option");
        assert_eq!(error.fallback_reason, "query_without_required_trigram");

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
            QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Regex { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected exact plan"),
        }
    }

    #[test]
    fn fixed_ascii_word_regexp_literal_is_memory_eligible() {
        let req = SearchRequest {
            pattern: "foo".to_string(),
            path: Some(".".to_string()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
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
        let plan = eligible_query_plan(&req).expect("ASCII fixed word literal should be eligible");
        match plan {
            QueryPlan::WordExact { literal, case } => {
                assert_eq!(literal, b"foo");
                assert_eq!(case, LiteralCase::Sensitive);
            }
            QueryPlan::Exact { .. }
            | QueryPlan::ShortExact { .. }
            | QueryPlan::Regex { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected word-exact plan"),
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
            QueryPlan::ShortExact { .. }
            | QueryPlan::WordExact { .. }
            | QueryPlan::Regex { .. }
            | QueryPlan::Fuzzy { .. } => panic!("expected exact plan"),
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
        let req = req.normalize();
        let limits = test_limits();
        let deadline = Instant::now() + Duration::from_secs(30);
        let snapshot = build_index(&req, &limits, deadline, false, 1).expect("index");
        let candidates =
            candidates_for_plan(&snapshot, &plan, &limits, deadline).expect("candidates");
        assert_eq!(candidates.len(), 1);

        let (events, _, _, _) = verify_and_render(
            &snapshot,
            &candidates,
            &plan,
            None,
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
        let req = req.normalize();
        let limits = test_limits();
        let deadline = Instant::now() + Duration::from_secs(30);
        let snapshot = build_index(&req, &limits, deadline, true, 1).expect("index");
        let candidates =
            candidates_for_plan(&snapshot, &plan, &limits, deadline).expect("candidates");
        let candidate_names: BTreeSet<String> = candidates
            .iter()
            .map(|doc_id| {
                snapshot.documents[doc_id.to_index()]
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
            None,
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
            fuzzy_seed_segments("abcdefgh", 1).expect("balanced ascii"),
            vec![b"abcd".to_vec(), b"efgh".to_vec()]
        );
        assert_eq!(
            fuzzy_seed_segments("abcdefghi", 2).expect("seedable ascii"),
            vec![b"abc".to_vec(), b"def".to_vec(), b"ghi".to_vec()]
        );
        assert_eq!(
            fuzzy_seed_segments("éabcé", 1).expect("seedable UTF-8 edges"),
            vec!["éa".as_bytes().to_vec(), "bcé".as_bytes().to_vec()]
        );
        assert!(fuzzy_seed_segments("ééé", 1).is_none());
    }

    #[test]
    fn fuzzy_seed_selection_prefers_selective_partition_within_budget() {
        let plan = eligible_query_plan(&fuzzy_req("aaabbbccc", 1)).expect("eligible fuzzy plan");
        let snapshot = snapshot_with_postings(vec![
            (*b"aaa", vec![0, 1, 2, 3, 9]),
            (*b"aab", vec![0, 1, 2, 3, 9]),
            (*b"abb", vec![9]),
            (*b"bbb", vec![0, 1, 2, 3, 9]),
            (*b"bbc", vec![0, 1, 2, 3, 9]),
            (*b"bcc", vec![0, 1, 2, 3, 9]),
            (*b"ccc", vec![9]),
        ]);
        let mut limits = test_limits();
        limits.max_candidates = 2;
        let deadline = Instant::now() + Duration::from_secs(30);

        let selection =
            select_fuzzy_seed_plan(&snapshot, &plan, &limits, deadline).expect("seed selection");

        assert_eq!(selection.candidates, doc_ids(&[9]));
        assert!(!selection.candidate_seeds.contains(&b"aaa".to_vec()));
        assert!(selection.partition_count > 1);
    }

    #[test]
    fn fuzzy_repeated_pattern_deduplicates_candidate_seeds_but_keeps_verifier_offsets() {
        let plan = eligible_query_plan(&fuzzy_req("aaaaaaaa", 1)).expect("eligible fuzzy plan");
        let snapshot = snapshot_with_postings(vec![(*b"aaa", vec![0, 1, 2])]);
        let limits = test_limits();
        let deadline = Instant::now() + Duration::from_secs(30);

        let selection =
            select_fuzzy_seed_plan(&snapshot, &plan, &limits, deadline).expect("seed selection");

        assert_eq!(selection.candidate_seeds, vec![b"aaaa".to_vec()]);
        assert_eq!(selection.verifier_seeds.len(), 2);
        assert_eq!(selection.duplicate_seed_count, 1);
        assert_eq!(selection.candidates, doc_ids(&[0, 1, 2]));
    }

    #[test]
    fn fuzzy_seed_candidate_cache_reuses_repeated_seeds_across_partition_plans() {
        let plan = QueryPlan::Fuzzy {
            pattern_chars: "abcdef".chars().collect(),
            distance: 1,
            seed_plans: vec![
                FuzzySeedPlan {
                    partition_index: 0,
                    seeds: vec![b"abc".to_vec()],
                    verifier_seeds: Vec::new(),
                },
                FuzzySeedPlan {
                    partition_index: 1,
                    seeds: vec![b"abc".to_vec()],
                    verifier_seeds: Vec::new(),
                },
            ],
        };
        let snapshot = snapshot_with_postings(vec![(*b"abc", vec![0, 2])]);
        let limits = test_limits();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut fuzzy_candidate_cache = FuzzySeedCandidateCache::default();

        let selection = select_fuzzy_seed_plan_with_cache(
            &snapshot,
            &plan,
            &limits,
            deadline,
            &mut fuzzy_candidate_cache,
        )
        .expect("seed selection");

        assert_eq!(selection.candidates, doc_ids(&[0, 2]));
        assert_eq!(selection.partition_index, 0);
        assert_eq!(fuzzy_candidate_cache.candidates_by_seed.len(), 1);
    }

    #[test]
    fn fuzzy_verifier_accepts_insertion_deletion_and_substitution() {
        let pattern: Vec<char> = "abcdef".chars().collect();
        let deadline = Instant::now() + Duration::from_secs(30);

        assert!(fuzzy_line_matches("prefix abcXdef suffix", &pattern, 1, deadline).expect("match"));
        assert!(fuzzy_line_matches("prefix abdef suffix", &pattern, 1, deadline).expect("match"));
        assert!(fuzzy_line_matches("prefix abcxef suffix", &pattern, 1, deadline).expect("match"));
        assert!(
            !fuzzy_line_matches("prefix abXYef suffix", &pattern, 1, deadline).expect("no match")
        );
    }

    #[test]
    fn fuzzy_verifier_rejects_long_lines_without_seed_windows() {
        let pattern: Vec<char> = "abcdef".chars().collect();
        let deadline = Instant::now() + Duration::from_secs(30);
        let line = "x".repeat(4096);

        assert!(!fuzzy_line_matches(&line, &pattern, 1, deadline).expect("no match"));
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
        let req = req.normalize();
        let limits = test_limits();
        let deadline = Instant::now() + Duration::from_secs(30);
        let snapshot = build_index(&req, &limits, deadline, true, 1).expect("index");
        let candidates =
            candidates_for_plan(&snapshot, &plan, &limits, deadline).expect("candidates");
        let candidate_names: BTreeSet<String> = candidates
            .iter()
            .map(|doc_id| {
                snapshot.documents[doc_id.to_index()]
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

        let fuzzy_seed_selection =
            select_fuzzy_seed_plan(&snapshot, &plan, &limits, deadline).expect("seed selection");
        let (events, _, verification_stats, _) = verify_and_render(
            &snapshot,
            &candidates,
            &plan,
            Some(&fuzzy_seed_selection),
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

        assert_eq!(verification_stats.fuzzy_verified_lines, 4);
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
        let req = req.normalize();
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
    fn fuzzy_pattern_limit_is_enforced_before_seed_planning() {
        let req = fuzzy_req("abcdef", 1).normalize();
        let mut limits = test_limits();
        limits.max_fuzzy_pattern_chars = 5;

        let err = eligible_query_plan_with_limits(
            &req,
            &limits,
            Instant::now() + Duration::from_secs(30),
        )
        .expect_err("oversized fuzzy pattern should hit the configured verifier limit");

        assert_eq!(err.error_type, "resource_limit_exceeded");
        assert_eq!(err.fallback_reason, "max_fuzzy_pattern_chars_exceeded");
        assert!(err.fallback_allowed);
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
        let req = req.normalize();

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
        let req = req.normalize();

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
        let req = req.normalize();

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
        let req = req.normalize();

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
        let req = req.normalize();
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
    fn freshness_check_detects_added_file() {
        let root = workspace_test_dir("freshness_check_detects_added_file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        fs::write(root.join("sample.txt"), "needle\n").expect("write initial file");

        let req = memory_req(&root).normalize();
        let snapshot = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");

        fs::write(root.join("added.txt"), "needle added\n").expect("write added file");

        let err = check_snapshot_fresh(&req, &snapshot, Instant::now() + Duration::from_secs(30))
            .expect_err("freshness should fail");
        assert_eq!(err.error_type, "file_changed_during_verification");
        assert_eq!(err.fallback_reason, "file_set_changed");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn targeted_freshness_detects_modified_result_file() {
        let root = workspace_test_dir("targeted_freshness_detects_modified_result_file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write initial file");

        let req = memory_req(&file_path).normalize();
        let snapshot = build_index(
            &req,
            &test_limits(),
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

        let err = SnapshotValidation::targeted(&req, BTreeSet::from([DocId(0)]))
            .validate(&snapshot, false, Instant::now() + Duration::from_secs(30))
            .expect_err("targeted freshness should fail");
        assert_eq!(err.error_type, "file_changed_during_verification");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn targeted_freshness_detects_deleted_result_file() {
        let root = workspace_test_dir("targeted_freshness_detects_deleted_result_file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write initial file");

        let req = memory_req(&file_path).normalize();
        let snapshot = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");

        fs::remove_file(&file_path).expect("delete result file");

        let err = SnapshotValidation::targeted(&req, BTreeSet::from([DocId(0)]))
            .validate(&snapshot, false, Instant::now() + Duration::from_secs(30))
            .expect_err("targeted freshness should fail");
        assert_eq!(err.error_type, "file_changed_during_verification");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn targeted_freshness_detects_added_file_with_full_scope_fallback() {
        let root = workspace_test_dir("targeted_freshness_detects_added_file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        fs::write(root.join("sample.txt"), "needle\n").expect("write initial file");

        let req = memory_req(&root).normalize();
        let snapshot = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");

        fs::write(root.join("added.txt"), "needle added\n").expect("write added file");

        let err = SnapshotValidation::targeted(&req, BTreeSet::new())
            .validate(&snapshot, false, Instant::now() + Duration::from_secs(30))
            .expect_err("targeted freshness should fall back to full-scope failure");
        assert_eq!(err.error_type, "file_changed_during_verification");
        assert_eq!(err.fallback_reason, "file_set_changed");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stable_ignore_rules_allow_targeted_default_ignore_validation() {
        let _env_guard = force_full_scope_env(None);
        let root = workspace_test_dir("stable_ignore_rules_allow_targeted_default_ignore");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");
        fs::write(root.join(".gitignore"), "# stable\n").expect("write gitignore");

        let mut req = memory_req(&root);
        req.hidden = Some(false);
        req.no_ignore = Some(false);
        let req = req.normalize();
        let snapshot = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");

        let result = SnapshotValidation::targeted(&req, BTreeSet::from([DocId(0)]))
            .validate(&snapshot, false, Instant::now() + Duration::from_secs(30))
            .expect("targeted freshness");

        assert_eq!(result.scope, SnapshotValidationScope::TargetedResultFiles);
        assert_eq!(result.full_scan_reason, None);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn mutated_gitignore_contents_trigger_ignore_full_scope_validation() {
        let _env_guard = force_full_scope_env(None);
        let root = workspace_test_dir("mutated_gitignore_contents_trigger_full_scope");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");
        fs::write(root.join(".gitignore"), "# before\n").expect("write gitignore");

        let mut req = memory_req(&root);
        req.hidden = Some(false);
        req.no_ignore = Some(false);
        let req = req.normalize();
        let snapshot = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");

        fs::write(root.join(".gitignore"), "# after\n").expect("mutate gitignore");

        let result = SnapshotValidation::targeted(&req, BTreeSet::from([DocId(0)]))
            .validate(&snapshot, false, Instant::now() + Duration::from_secs(30))
            .expect("full-scope freshness");

        assert_eq!(result.scope, SnapshotValidationScope::FullScope);
        assert_eq!(result.full_scan_reason, Some("gitignore_changed"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn force_full_scope_on_ignore_env_restores_conservative_validation() {
        let _env_guard = force_full_scope_env(Some("1"));

        let root = workspace_test_dir("force_full_scope_on_ignore_env");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");

        let mut req = memory_req(&root);
        req.hidden = Some(false);
        req.no_ignore = Some(false);
        let req = req.normalize();
        let snapshot = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");

        let result = SnapshotValidation::targeted(&req, BTreeSet::from([DocId(0)]))
            .validate(&snapshot, false, Instant::now() + Duration::from_secs(30))
            .expect("forced full-scope freshness");

        assert_eq!(result.scope, SnapshotValidationScope::FullScope);
        assert_eq!(
            result.full_scan_reason,
            Some("ignore_rules_forced_full_scope")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn memory_search_rejects_stale_success_after_added_matching_file() {
        let root = workspace_test_dir("memory_search_rejects_stale_success_after_added_file");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        fs::write(root.join("sample.txt"), "haystack\n").expect("write initial file");

        let req = memory_req(&root).normalize();
        let first = handle_memory_search(&req)
            .await
            .expect("initial memory search");
        assert_eq!(first.0["isError"], false);
        assert_eq!(first.0["count"], 0);

        fs::write(root.join("added.txt"), "needle added\n").expect("write added file");

        match handle_memory_search(&req).await {
            Ok(outcome) => {
                let payload = outcome.0;
                assert_eq!(payload["isError"], false);
                assert!(
                    payload["count"].as_u64().expect("count") > 0,
                    "cache miss or rebuild must surface the added match instead of stale no-match success"
                );
            }
            Err(err) => {
                assert_eq!(err.error_type, "file_changed_during_verification");
                assert_eq!(err.fallback_reason, "file_set_changed");
            }
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn unchanged_metadata_fast_path_skips_byte_verification_when_marker_available() {
        let root = workspace_test_dir("unchanged_metadata_fast_path_skips_byte_verification");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write initial file");

        let metadata = fs::metadata(&file_path).expect("metadata");
        let stamp = file_stamp_from_parts(&metadata);
        let metadata = fs::metadata(&file_path).expect("metadata again");

        assert!(file_metadata_matches_without_hash(&stamp, &metadata));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(windows)]
    fn windows_unchanged_metadata_requires_byte_validation() {
        let root = workspace_test_dir("windows_unchanged_metadata_requires_byte_validation");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write initial file");

        let metadata = fs::metadata(&file_path).expect("metadata");
        let stamp = file_stamp_from_parts(&metadata);
        let metadata = fs::metadata(&file_path).expect("metadata again");

        assert!(!metadata_stamp_can_validate_without_hash(&stamp));
        assert!(!file_metadata_matches_without_hash(&stamp, &metadata));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn freshness_byte_validation_rejects_same_length_content_change() {
        let root =
            workspace_test_dir("freshness_byte_validation_rejects_same_length_content_change");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write initial file");

        let metadata = fs::metadata(&file_path).expect("metadata");
        let content = fs::read(&file_path).expect("read");
        let mut doc = Document {
            path: file_path.clone(),
            rendered_path: file_path.display().to_string(),
            stamp: file_stamp_from_parts(&metadata),
            content,
            lines: Vec::new(),
        };
        doc.stamp.modified = fs::metadata(&file_path)
            .expect("metadata again")
            .modified()
            .ok();
        doc.stamp.change_marker =
            metadata_change_marker(&fs::metadata(&file_path).expect("metadata again"));

        fs::write(&file_path, "change\n").expect("same-length rewrite");
        let metadata = fs::metadata(&file_path).expect("changed metadata");
        doc.stamp.len = metadata.len();
        doc.stamp.modified = metadata.modified().ok();
        doc.stamp.change_marker = metadata_change_marker(&metadata);

        let err = validate_result_file_content_matches(
            &doc,
            &metadata,
            Instant::now() + Duration::from_secs(30),
        )
        .expect_err("byte mismatch should fail validation");
        assert_eq!(err.error_type, "file_changed_during_verification");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(windows)]
    #[allow(clippy::permissions_set_readonly_false)]
    fn windows_metadata_marker_tracks_file_attribute_changes() {
        let root = workspace_test_dir("windows_metadata_marker_tracks_file_attribute_changes");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write initial file");

        let initial = metadata_change_marker(&fs::metadata(&file_path).expect("metadata"))
            .expect("windows marker");
        let mut permissions = fs::metadata(&file_path)
            .expect("metadata for permissions")
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file_path, permissions).expect("set readonly");

        let readonly =
            metadata_change_marker(&fs::metadata(&file_path).expect("readonly metadata"))
                .expect("readonly marker");
        assert_ne!(initial, readonly);

        let mut permissions = fs::metadata(&file_path)
            .expect("metadata for restore")
            .permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&file_path, permissions).expect("restore writable");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn unix_metadata_marker_captures_identity_mode_and_ctime() {
        use std::os::unix::fs::MetadataExt as _;

        let root = workspace_test_dir("unix_metadata_marker_captures_identity_mode_and_ctime");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write fixture");

        let metadata = fs::metadata(&file_path).expect("metadata");
        let marker = metadata_change_marker(&metadata).expect("unix marker");

        assert_eq!(marker.dev, metadata.dev());
        assert_eq!(marker.ino, metadata.ino());
        assert_eq!(marker.mode, metadata.mode());
        assert_eq!(marker.ctime, metadata.ctime());
        assert_eq!(marker.ctime_nsec, metadata.ctime_nsec());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn unix_metadata_marker_tracks_permission_changes() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = workspace_test_dir("unix_metadata_marker_tracks_permission_changes");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let file_path = root.join("sample.txt");
        fs::write(&file_path, "needle\n").expect("write fixture");

        let initial = metadata_change_marker(&fs::metadata(&file_path).expect("initial metadata"))
            .expect("unix marker");
        let mut permissions = fs::metadata(&file_path)
            .expect("metadata for permissions")
            .permissions();
        permissions.set_mode(permissions.mode() ^ 0o100);
        fs::set_permissions(&file_path, permissions).expect("toggle executable bit");

        let changed = metadata_change_marker(&fs::metadata(&file_path).expect("changed metadata"))
            .expect("changed unix marker");

        assert_ne!(initial, changed);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn unix_targeted_freshness_full_scans_after_non_result_permission_change() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = workspace_test_dir("unix_targeted_freshness_full_scans_after_permission_change");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let match_path = root.join("match.txt");
        let other_path = root.join("other.txt");
        fs::write(&match_path, "needle\n").expect("write matching file");
        fs::write(&other_path, "haystack\n").expect("write non-result file");

        let req = memory_req(&root).normalize();
        let snapshot = build_index(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
            1,
        )
        .expect("index");
        let result_doc_id = snapshot
            .documents
            .iter()
            .position(|doc| {
                doc.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == "match.txt")
            })
            .map(DocId::from_index)
            .transpose()
            .expect("doc id")
            .expect("match document");

        let mut permissions = fs::metadata(&other_path)
            .expect("metadata for permissions")
            .permissions();
        permissions.set_mode(permissions.mode() ^ 0o100);
        fs::set_permissions(&other_path, permissions).expect("toggle executable bit");

        let result = SnapshotValidation::targeted(&req, BTreeSet::from([result_doc_id]))
            .validate(&snapshot, false, Instant::now() + Duration::from_secs(30))
            .expect("same-content metadata change should full-scope validate");

        assert_eq!(result.scope, SnapshotValidationScope::FullScope);
        assert_eq!(
            result.full_scan_reason,
            Some("indexed_file_metadata_changed")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    async fn unix_memory_search_renders_non_utf8_paths_lossily() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let root = workspace_test_dir("unix_memory_search_renders_non_utf8_paths_lossily");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create test dir");
        let filename = OsString::from_vec(b"nonutf8-\xff.txt".to_vec());
        fs::write(root.join(filename), "needle\n").expect("write non-UTF-8 filename");

        let req = memory_req(&root).normalize();
        let outcome = handle_memory_search(&req)
            .await
            .expect("memory search should support non-UTF-8 paths");
        let payload = outcome.0;
        let path_text = payload["matches"][0]["data"]["path"]["text"]
            .as_str()
            .expect("path text");

        assert_eq!(payload["backend"], "memory");
        assert!(
            path_text.contains("nonutf8-") && path_text.contains('\u{fffd}'),
            "expected lossy rendered path, got: {path_text}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_index_cache_reuses_snapshot_for_same_file_selection() {
        let root = workspace_test_dir("search_index_cache_reuses_snapshot_for_same_file_selection");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");

        let req = memory_req(&root).normalize();
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
    fn concurrent_same_key_cold_misses_share_single_build() {
        let root = workspace_test_dir("concurrent_same_key_cold_misses_share_single_build");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");

        let req = memory_req(&root).normalize();
        let hook_root = req.root().to_string();
        let probe: DedupeProbe =
            Arc::new((Mutex::new(DedupeProbeState::default()), Condvar::new()));
        let build_count = Arc::new(AtomicUsize::new(0));

        let _guard = install_dedupe_probe_hooks(hook_root, &probe, &build_count);
        let limits = test_limits();
        let first_req = req.clone();
        let first_limits = limits.clone();
        let first = thread::spawn(move || {
            get_or_build_snapshot(
                &first_req,
                &first_limits,
                Instant::now() + Duration::from_secs(30),
                false,
            )
            .expect("first snapshot")
        });

        wait_for_probe(
            &probe,
            Duration::from_secs(5),
            "first build to enter",
            |state| state.first_build_entered,
        );

        let second_req = req.clone();
        let second_limits = limits.clone();
        let second = thread::spawn(move || {
            get_or_build_snapshot(
                &second_req,
                &second_limits,
                Instant::now() + Duration::from_secs(30),
                false,
            )
            .expect("second snapshot")
        });

        wait_for_probe(
            &probe,
            Duration::from_secs(5),
            "second caller to wait instead of build",
            |state| state.waiter_entered || state.build_count > 1,
        );
        let state = probe_state(&probe);
        if !state.waiter_entered || state.build_count != 1 {
            release_first_build(&probe);
            let _ = first.join();
            let _ = second.join();
            panic!(
                "expected second caller to wait on the in-flight build; waiter_entered={}, build_count={}",
                state.waiter_entered, state.build_count
            );
        }

        release_first_build(&probe);
        let first = first.join().expect("first thread");
        let second = second.join().expect("second thread");

        assert_eq!(build_count.load(Ordering::SeqCst), 1);
        assert_eq!(first.cache_status, "miss");
        assert_eq!(second.cache_status, "hit");
        assert!(!first.build_deduped);
        assert!(second.build_deduped);
        assert!(Arc::ptr_eq(&first.snapshot, &second.snapshot));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn waiter_retries_after_builder_timeout_without_reusing_timeout_error() {
        let root = workspace_test_dir(
            "waiter_retries_after_builder_timeout_without_reusing_timeout_error",
        );
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");

        let req = memory_req(&root).normalize();
        let hook_root = req.root().to_string();
        let probe: DedupeProbe =
            Arc::new((Mutex::new(DedupeProbeState::default()), Condvar::new()));
        let build_count = Arc::new(AtomicUsize::new(0));

        let _guard = install_dedupe_probe_hooks(hook_root, &probe, &build_count);
        let limits = test_limits();
        let first_req = req.clone();
        let first_limits = limits.clone();
        let first = thread::spawn(move || {
            get_or_build_snapshot(
                &first_req,
                &first_limits,
                Instant::now() + Duration::from_millis(25),
                false,
            )
        });

        wait_for_probe(
            &probe,
            Duration::from_secs(5),
            "first build to enter",
            |state| state.first_build_entered,
        );

        let second_req = req.clone();
        let second_limits = limits.clone();
        let second = thread::spawn(move || {
            get_or_build_snapshot(
                &second_req,
                &second_limits,
                Instant::now() + Duration::from_secs(30),
                false,
            )
        });

        wait_for_probe(
            &probe,
            Duration::from_secs(5),
            "second caller to wait on first build",
            |state| state.waiter_entered,
        );
        thread::sleep(Duration::from_millis(75));
        release_first_build(&probe);

        let first = first.join().expect("first thread");
        let second = second.join().expect("second thread");

        assert_timeout(first.expect_err("first build should time out"));
        let second = second.expect("second caller should retry and build");
        assert_eq!(build_count.load(Ordering::SeqCst), 2);
        assert_eq!(second.cache_status, "miss");
        assert!(!second.build_deduped);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_index_key_normalizes_reordered_duplicate_globs() {
        let root = workspace_test_dir("search_index_key_normalizes_reordered_duplicate_globs");
        let mut first = memory_req(&root);
        first.glob = Some(vec![
            " *.txt ".to_string(),
            "*.md".to_string(),
            "*.txt".to_string(),
            "  ".to_string(),
        ]);
        let mut second = memory_req(&root);
        second.glob = Some(vec!["*.md".to_string(), "*.txt".to_string()]);
        let first = first.normalize();
        let second = second.normalize();

        let first_selector = FileSelector::for_memory(&first).expect("first selector");
        let second_selector = FileSelector::for_memory(&second).expect("second selector");

        assert_eq!(
            IndexKey::from_selector(&first_selector),
            IndexKey::from_selector(&second_selector)
        );
    }

    #[test]
    fn search_index_key_normalizes_equivalent_path_globs_without_colliding_with_basename() {
        let root = workspace_test_dir(
            "search_index_key_normalizes_equivalent_path_globs_without_collision",
        );
        let mut anchored = memory_req(&root);
        anchored.glob = Some(vec![" ./root.rs ".to_string(), "/root.rs".to_string()]);
        let mut equivalent = memory_req(&root);
        equivalent.glob = Some(vec!["/root.rs".to_string()]);
        let mut basename = memory_req(&root);
        basename.glob = Some(vec!["root.rs".to_string()]);

        let anchored = FileSelector::for_memory(&anchored.normalize()).expect("anchored selector");
        let equivalent =
            FileSelector::for_memory(&equivalent.normalize()).expect("equivalent selector");
        let basename = FileSelector::for_memory(&basename.normalize()).expect("basename selector");

        assert_eq!(
            IndexKey::from_selector(&anchored),
            IndexKey::from_selector(&equivalent)
        );
        assert_ne!(
            IndexKey::from_selector(&anchored),
            IndexKey::from_selector(&basename)
        );
    }

    #[test]
    fn search_index_cache_reuses_snapshot_for_equivalent_glob_sets() {
        let root =
            workspace_test_dir("search_index_cache_reuses_snapshot_for_equivalent_glob_sets");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("sample.txt"), "needle\n").expect("write txt fixture");
        fs::write(root.join("sample.md"), "needle\n").expect("write md fixture");

        let mut first_req = memory_req(&root);
        first_req.glob = Some(vec!["*.txt".to_string(), "*.md".to_string()]);
        let mut second_req = memory_req(&root);
        second_req.glob = Some(vec![
            "*.md".to_string(),
            " *.txt ".to_string(),
            "*.md".to_string(),
        ]);
        let first_req = first_req.normalize();
        let second_req = second_req.normalize();
        let limits = test_limits();
        let first = get_or_build_snapshot(
            &first_req,
            &limits,
            Instant::now() + Duration::from_secs(30),
            false,
        )
        .expect("first snapshot");
        let second = get_or_build_snapshot(
            &second_req,
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
    fn search_index_cache_caps_total_entries() {
        let root = workspace_test_dir("search_index_cache_caps_total_entries");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");

        for idx in 0..=DEFAULT_INDEX_CACHE_MAX_ENTRIES {
            let mut req = memory_req(&root);
            req.glob = Some(vec![format!("nomatch_{idx}")]);
            let req = req.normalize();
            let _ = get_or_build_snapshot(
                &req,
                &test_limits(),
                Instant::now() + Duration::from_secs(30),
                false,
            )
            .expect("cached snapshot");
        }

        assert!(lock_index_manager().ready_entry_count() <= DEFAULT_INDEX_CACHE_MAX_ENTRIES);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn index_cache_eviction_keeps_recently_touched_entries() {
        let mut cache = IndexManager::default();
        let keys: Vec<IndexKey> = (0..=DEFAULT_INDEX_CACHE_MAX_ENTRIES)
            .map(|idx| IndexKey {
                root: format!("root-{idx}"),
                hidden: false,
                follow: false,
                no_ignore: false,
                globs: Vec::new(),
            })
            .collect();

        for key in &keys[..DEFAULT_INDEX_CACHE_MAX_ENTRIES] {
            cache
                .entries
                .insert(key.clone(), IndexEntry::ready(Arc::new(empty_snapshot(1))));
            cache.touch(key);
        }
        cache.touch(&keys[0]);
        cache.entries.insert(
            keys[DEFAULT_INDEX_CACHE_MAX_ENTRIES].clone(),
            IndexEntry::ready(Arc::new(empty_snapshot(2))),
        );
        cache.touch(&keys[DEFAULT_INDEX_CACHE_MAX_ENTRIES]);
        cache.evict_to_capacity(IndexCacheLimits {
            max_entries: DEFAULT_INDEX_CACHE_MAX_ENTRIES,
            max_bytes: None,
        });

        assert!(cache.entries.contains_key(&keys[0]));
        assert!(!cache.entries.contains_key(&keys[1]));
        assert!(
            cache
                .entries
                .contains_key(&keys[DEFAULT_INDEX_CACHE_MAX_ENTRIES])
        );
        assert_eq!(cache.cache_evictions, 1);
    }

    #[test]
    fn index_cache_hit_promotes_entry_before_entry_cap_eviction() {
        let mut cache = IndexManager::default();
        let limits = IndexCacheLimits {
            max_entries: 2,
            max_bytes: None,
        };
        let first = index_key_for_test("lru-first");
        let second = index_key_for_test("lru-second");
        let third = index_key_for_test("lru-third");

        cache.publish_snapshot_if_absent_with_limits(
            first.clone(),
            Arc::new(empty_snapshot(1)),
            limits,
        );
        cache.publish_snapshot_if_absent_with_limits(
            second.clone(),
            Arc::new(empty_snapshot(2)),
            limits,
        );
        assert!(cache.cached_snapshot_with_limits(&first, limits).is_some());
        cache.publish_snapshot_if_absent_with_limits(
            third.clone(),
            Arc::new(empty_snapshot(3)),
            limits,
        );

        assert!(cache.entries.contains_key(&first));
        assert!(!cache.entries.contains_key(&second));
        assert!(cache.entries.contains_key(&third));
        assert_eq!(cache.cache_evictions, 1);
    }

    #[test]
    fn index_cache_eviction_honors_byte_cap() {
        let mut cache = IndexManager::default();
        let limits = IndexCacheLimits {
            max_entries: 8,
            max_bytes: Some(40),
        };
        let first = index_key_for_test("byte-first");
        let second = index_key_for_test("byte-second");
        let third = index_key_for_test("byte-third");

        cache.publish_snapshot_if_absent_with_limits(
            first.clone(),
            Arc::new(empty_snapshot_with_bytes(1, 20)),
            limits,
        );
        cache.publish_snapshot_if_absent_with_limits(
            second.clone(),
            Arc::new(empty_snapshot_with_bytes(2, 20)),
            limits,
        );
        assert!(cache.cached_snapshot_with_limits(&second, limits).is_some());
        cache.publish_snapshot_if_absent_with_limits(
            third.clone(),
            Arc::new(empty_snapshot_with_bytes(3, 15)),
            limits,
        );

        let telemetry = cache.cache_telemetry(limits);
        assert!(!cache.entries.contains_key(&first));
        assert!(cache.entries.contains_key(&second));
        assert!(cache.entries.contains_key(&third));
        assert_eq!(telemetry.entries, 2);
        assert_eq!(telemetry.bytes, 35);
        assert!(telemetry.bytes <= limits.max_bytes.expect("byte cap"));
        assert_eq!(telemetry.evictions, 1);
    }

    #[test]
    fn index_manager_transitions_cold_building_ready_and_hit() {
        let mut manager = IndexManager::default();
        let key = IndexKey {
            root: "state-root".to_string(),
            hidden: false,
            follow: false,
            no_ignore: false,
            globs: Vec::new(),
        };

        assert_eq!(manager.state_for(&key), IndexEntryState::Cold);
        let reservation = manager.begin_build(&key);
        assert_eq!(reservation.generation, 1);
        assert_eq!(manager.state_for(&key), IndexEntryState::Building);

        let snapshot = Arc::new(empty_snapshot(reservation.generation));
        let published = manager.publish_snapshot_if_absent(key.clone(), snapshot.clone());

        assert!(Arc::ptr_eq(&published, &snapshot));
        assert_eq!(manager.state_for(&key), IndexEntryState::Ready);
        assert_eq!(manager.ready_entry_count(), 1);
        assert!(Arc::ptr_eq(
            &manager.cached_snapshot(&key).expect("ready snapshot"),
            &snapshot
        ));
    }

    #[test]
    fn index_manager_records_unavailable_and_allows_retry() {
        let mut manager = IndexManager::default();
        let key = IndexKey {
            root: "unavailable-root".to_string(),
            hidden: false,
            follow: false,
            no_ignore: false,
            globs: Vec::new(),
        };

        let reservation = manager.begin_build(&key);
        let err = MemoryError::new(
            "search_index_unavailable",
            "search_index_unavailable",
            "index unavailable",
        );
        manager.record_build_failure(&key, reservation, &err);

        let entry = manager.entries.get(&key).expect("unavailable entry");
        assert_eq!(entry.state, IndexEntryState::Unavailable);
        assert_eq!(entry.last_error_type, Some("search_index_unavailable"));
        assert_eq!(entry.last_fallback_reason, Some("search_index_unavailable"));
        assert!(manager.cached_snapshot(&key).is_none());

        let retry = manager.begin_build(&key);
        assert_eq!(retry.generation, reservation.generation + 1);
        assert_eq!(manager.state_for(&key), IndexEntryState::Building);
    }

    #[test]
    fn index_manager_does_not_cache_per_request_build_failures() {
        let mut manager = IndexManager::default();
        let key = IndexKey {
            root: "timeout-root".to_string(),
            hidden: false,
            follow: false,
            no_ignore: false,
            globs: Vec::new(),
        };

        let reservation = manager.begin_build(&key);
        let err = MemoryError::timeout();
        manager.record_build_failure(&key, reservation, &err);

        let entry = manager.entries.get(&key).expect("cold entry");
        assert_eq!(entry.state, IndexEntryState::Cold);
        assert!(entry.last_error.is_none());
        assert!(entry.last_error_type.is_none());
        assert!(entry.last_fallback_reason.is_none());
        assert!(manager.cached_snapshot(&key).is_none());

        let retry = manager.begin_build(&key);
        assert_eq!(retry.generation, reservation.generation + 1);
        assert_eq!(manager.state_for(&key), IndexEntryState::Building);
    }

    #[test]
    fn index_manager_validation_transitions_refreshing_ready_and_unavailable() {
        let mut manager = IndexManager::default();
        let key = IndexKey {
            root: "validation-root".to_string(),
            hidden: false,
            follow: false,
            no_ignore: false,
            globs: Vec::new(),
        };
        let snapshot = Arc::new(empty_snapshot(7));
        manager.publish_snapshot_if_absent(key.clone(), snapshot.clone());

        assert!(!manager.begin_validation(&key, &snapshot));
        assert_eq!(manager.state_for(&key), IndexEntryState::Refreshing);
        assert!(manager.cached_snapshot(&key).is_some());

        assert_eq!(
            manager.complete_validation(&key, &snapshot, false),
            IndexEntryState::Ready
        );
        assert_eq!(manager.state_for(&key), IndexEntryState::Ready);

        assert!(!manager.begin_validation(&key, &snapshot));
        let err = MemoryError::new(
            "file_changed_during_verification",
            "file_set_changed",
            "file set changed",
        );
        manager.record_validation_failure(&key, &snapshot, &err);

        assert_eq!(manager.state_for(&key), IndexEntryState::Unavailable);
        assert!(manager.cached_snapshot(&key).is_none());
        assert_eq!(manager.ready_entry_count(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn memory_search_truncates_context_events_as_success() {
        let root = workspace_test_dir("memory_search_truncates_context_events_as_success");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(
            root.join("sample.txt"),
            "alpha\nneedle one\nmiddle\nneedle two\nomega\nneedle three\n",
        )
        .expect("write fixture");

        let mut req = memory_req(&root);
        req.context = Some(1);
        req.max_results = Some(3);
        let req = req.normalize();

        let outcome = handle_memory_search(&req).await.expect("memory search");
        let payload = outcome.0;

        assert_eq!(payload["isError"], false);
        assert_eq!(payload["backend"], "memory");
        assert_eq!(payload["plan_kind"], "exact");
        assert_eq!(payload["memory_eligibility"], "eligible");
        assert_eq!(payload["truncated"], true);
        assert_eq!(payload["timed_out"], false);
        assert_eq!(payload["count"], 3);
        assert_eq!(payload["candidate_seed_count"], 4);
        assert_eq!(payload["candidate_count"], 1);
        assert!(
            payload["verified_line_count"]
                .as_u64()
                .expect("verified lines")
                < 6,
            "verification should stop once the truncated prefix is known"
        );
        assert_eq!(payload["max_results_reached"], true);
        assert_eq!(payload["freshness_check"], "verified");
        assert_eq!(payload["matches"].as_array().expect("matches").len(), 3);
        let text = payload["content"][0]["text"].as_str().expect("text");
        assert_eq!(text.lines().count(), 3);

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
    fn parse_git_toplevel_output_trims_crlf_and_ignores_empty_lines() {
        assert_eq!(
            parse_git_toplevel_output(b"\r\nC:/repo/root\r\n"),
            Some(PathBuf::from("C:/repo/root"))
        );
        assert_eq!(parse_git_toplevel_output(b"\r\n"), None);
    }

    #[test]
    fn warm_cache_globs_from_raw_preserves_priority_dedupes_and_filters_none() {
        assert_eq!(
            warm_cache_globs_from_raw("*.md, none; *.rs ;*.md,,"),
            vec!["*.md".to_string(), "*.rs".to_string()]
        );
    }

    #[test]
    fn likely_warm_cache_keys_include_repo_cwd_and_bounded_globs() {
        let repo_root = PathBuf::from("repo-root");
        let cwd = repo_root.join("src");
        let config = WarmCacheConfig {
            enabled: true,
            start_delay: Duration::ZERO,
            key_delay: Duration::ZERO,
            timeout_ms: 30_000,
            max_keys: 3,
            globs: vec!["*.rs".to_string(), "*.md".to_string()],
            git_timeout: Duration::from_millis(100),
        };

        let keys = likely_warm_cache_keys(&cwd, &repo_root, &config);

        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].root, "repo-root");
        assert!(keys[0].globs.is_empty());
        assert_eq!(keys[1].root, ".");
        assert!(keys[1].globs.is_empty());
        assert_eq!(keys[2].root, "repo-root");
        assert_eq!(keys[2].globs, vec!["*.rs".to_string()]);
    }

    #[test]
    fn likely_warm_cache_keys_cover_no_glob_and_cwd_globs_within_cap() {
        let repo_root = PathBuf::from("repo-root");
        let cwd = repo_root.join("src");
        let config = WarmCacheConfig {
            enabled: true,
            start_delay: Duration::ZERO,
            key_delay: Duration::ZERO,
            timeout_ms: 30_000,
            max_keys: 6,
            globs: vec!["*.rs".to_string(), "*.md".to_string()],
            git_timeout: Duration::from_millis(100),
        };

        let keys = likely_warm_cache_keys(&cwd, &repo_root, &config);

        assert_eq!(keys.len(), 6);
        assert_eq!(keys[0].root, "repo-root");
        assert!(
            keys[0].globs.is_empty(),
            "first key is repo-default no-glob"
        );
        assert_eq!(keys[1].root, ".");
        assert!(keys[1].globs.is_empty(), "cwd-default no-glob present");
        assert_eq!(keys[2].root, "repo-root");
        assert_eq!(keys[2].globs, vec!["*.rs".to_string()]);
        assert_eq!(keys[3].root, "repo-root");
        assert_eq!(keys[3].globs, vec!["*.md".to_string()]);
        assert_eq!(keys[4].root, ".");
        assert_eq!(keys[4].globs, vec!["*.rs".to_string()]);
        assert_eq!(keys[5].root, ".");
        assert_eq!(keys[5].globs, vec!["*.md".to_string()]);
    }

    #[test]
    fn likely_warm_cache_keys_dedup_when_cwd_matches_repo_root() {
        let repo_root = PathBuf::from("repo-root");
        let config = WarmCacheConfig {
            enabled: true,
            start_delay: Duration::ZERO,
            key_delay: Duration::ZERO,
            timeout_ms: 30_000,
            max_keys: 6,
            globs: vec!["*.rs".to_string(), "*.md".to_string()],
            git_timeout: Duration::from_millis(100),
        };

        let keys = likely_warm_cache_keys(&repo_root, &repo_root, &config);

        assert_eq!(keys.len(), 3, "no cwd duplicates when cwd == repo_root");
        assert_eq!(keys[0].root, ".");
        assert!(keys[0].globs.is_empty());
        assert_eq!(keys[1].root, ".");
        assert_eq!(keys[1].globs, vec!["*.rs".to_string()]);
        assert_eq!(keys[2].root, ".");
        assert_eq!(keys[2].globs, vec!["*.md".to_string()]);
    }

    #[test]
    fn warm_cache_for_key_populates_likely_glob_cache_key() {
        let root = workspace_test_dir("warm_cache_for_key_populates_likely_glob_cache_key");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("cached.rs"), "cachedneedle\n").expect("write rs fixture");
        fs::write(root.join("cached.txt"), "cachedneedle\n").expect("write txt fixture");

        let warm_key = WarmCacheKey {
            root: root.display().to_string(),
            globs: vec!["*.rs".to_string()],
            label: "repo-glob",
        };
        let summary = warm_cache_for_key(&warm_key, 30_000).expect("warm cache should build");

        let mut req = memory_req(&root);
        req.pattern = "cachedneedle".to_string();
        req.glob = Some(vec!["*.rs".to_string()]);
        req.hidden = Some(false);
        req.follow = Some(false);
        req.no_ignore = Some(false);
        req.word_regexp = Some(false);
        let req = req.normalize();
        let cached = get_or_build_snapshot(
            &req,
            &test_limits(),
            Instant::now() + Duration::from_secs(30),
            false,
        )
        .expect("query should use warmed glob cache");

        assert_eq!(summary.globs, vec!["*.rs".to_string()]);
        assert_eq!(cached.cache_status, "hit");
        assert_eq!(cached.snapshot.generation, summary.generation);
        assert_eq!(cached.snapshot.documents.len(), 1);
        assert!(
            cached
                .snapshot
                .documents
                .iter()
                .any(|doc| { doc.path.file_name().is_some_and(|name| name == "cached.rs") })
        );

        let _ = fs::remove_dir_all(&root);
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
        let req = req.normalize();
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

    #[test]
    fn warm_cache_waits_for_in_flight_query_build_for_same_key() {
        let root = workspace_test_dir("warm_cache_waits_for_in_flight_query_build_for_same_key");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("cached.txt"), "cachedneedle\n").expect("write fixture");

        let root_arg = root.display().to_string();
        let req = SearchRequest {
            pattern: "cachedneedle".to_string(),
            path: Some(root_arg.clone()),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: Some(false),
            glob: None,
            hidden: Some(false),
            follow: Some(false),
            no_ignore: Some(false),
            context: None,
            max_results: None,
            timeout_ms: Some(30_000),
            fuzzy: None,
        }
        .normalize();
        let hook_root = req.root().to_string();
        let probe: DedupeProbe =
            Arc::new((Mutex::new(DedupeProbeState::default()), Condvar::new()));
        let build_count = Arc::new(AtomicUsize::new(0));
        let _guard = install_dedupe_probe_hooks(hook_root, &probe, &build_count);

        let limits = test_limits();
        let query_req = req.clone();
        let query = thread::spawn(move || {
            get_or_build_snapshot(
                &query_req,
                &limits,
                Instant::now() + Duration::from_secs(30),
                false,
            )
            .expect("query snapshot")
        });

        wait_for_probe(
            &probe,
            Duration::from_secs(5),
            "query build to enter",
            |state| state.first_build_entered,
        );

        let warm = thread::spawn(move || warm_cache_for_root(root_arg).expect("warm cache"));
        wait_for_probe(
            &probe,
            Duration::from_secs(5),
            "warm cache to wait instead of build",
            |state| state.waiter_entered || state.build_count > 1,
        );
        let state = probe_state(&probe);
        if !state.waiter_entered || state.build_count != 1 {
            release_first_build(&probe);
            let _ = query.join();
            let _ = warm.join();
            panic!(
                "expected warm cache to wait on the in-flight query build; waiter_entered={}, build_count={}",
                state.waiter_entered, state.build_count
            );
        }

        release_first_build(&probe);
        let query = query.join().expect("query thread");
        let warm = warm.join().expect("warm thread");

        assert_eq!(build_count.load(Ordering::SeqCst), 1);
        assert_eq!(query.cache_status, "miss");
        assert!(!query.build_deduped);
        assert_eq!(warm.generation, query.snapshot.generation);
        assert_eq!(warm.indexed_files, query.snapshot.documents.len());
        assert_eq!(warm.indexed_bytes, query.snapshot.indexed_bytes);

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
            max_short_literal_scan_lines: DEFAULT_MAX_SHORT_LITERAL_SCAN_LINES,
            regex_size_limit_bytes: DEFAULT_REGEX_SIZE_LIMIT_BYTES,
        }
    }

    fn assert_timeout(err: MemoryError) {
        assert_eq!(err.error_type, "query_timeout");
        assert_eq!(err.fallback_reason, "query_timeout");
        assert!(!err.fallback_allowed);
        assert!(err.timed_out);
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

    fn index_key_for_test(root: &str) -> IndexKey {
        IndexKey {
            root: root.to_string(),
            hidden: false,
            follow: false,
            no_ignore: false,
            globs: Vec::new(),
        }
    }

    fn empty_snapshot(generation: u64) -> IndexSnapshot {
        empty_snapshot_with_bytes(generation, 0)
    }

    fn empty_snapshot_with_bytes(generation: u64, indexed_bytes: u64) -> IndexSnapshot {
        IndexSnapshot {
            generation,
            documents: Vec::new(),
            scope_fingerprint: ScopeFingerprint::default(),
            ignore_fingerprint: None,
            postings: PostingsIndex::default(),
            ascii_folded_postings: PostingsIndex::default(),
            indexed_bytes,
            all_content_utf8: true,
        }
    }

    fn document_for_test(name: &str, content: &[u8]) -> Document {
        Document {
            path: PathBuf::from(name),
            rendered_path: name.to_string(),
            stamp: FileStamp {
                len: content.len() as u64,
                modified: None,
                change_marker: None,
            },
            content: content.to_vec(),
            lines: line_ranges(content),
        }
    }

    fn doc_ids(indices: &[usize]) -> Vec<DocId> {
        indices
            .iter()
            .copied()
            .map(|index| DocId::from_index(index).expect("test doc id"))
            .collect()
    }

    fn snapshot_with_postings(entries: Vec<([u8; 3], Vec<usize>)>) -> IndexSnapshot {
        let mut snapshot = empty_snapshot(1);
        let mut postings = HashMap::new();
        for (trigram, indices) in entries {
            postings.insert(trigram, postings_for_test(&indices));
        }
        snapshot.postings = PostingsIndex { entries: postings };
        snapshot
    }

    fn postings_for_test(indices: &[usize]) -> Postings {
        Postings::from_doc_ids(doc_ids(indices), Instant::now() + Duration::from_secs(30))
            .expect("test postings")
    }

    type RawPostingsForTest = HashMap<[u8; 3], Vec<DocId>>;

    fn legacy_document_trigrams(
        doc_id: DocId,
        content: &[u8],
    ) -> (RawPostingsForTest, RawPostingsForTest) {
        let mut postings = HashMap::new();
        let mut folded_postings = HashMap::new();
        for trigram in literal_trigrams(content) {
            postings
                .entry(trigram)
                .or_insert_with(Vec::new)
                .push(doc_id);
        }
        let folded = content
            .iter()
            .map(u8::to_ascii_lowercase)
            .collect::<Vec<_>>();
        for trigram in literal_trigrams(&folded) {
            folded_postings
                .entry(trigram)
                .or_insert_with(Vec::new)
                .push(doc_id);
        }
        (postings, folded_postings)
    }

    #[test]
    fn check_cancellation_errors_when_token_cancelled() {
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let err = check_cancellation(Some(&token)).expect_err("expected cancelled error");
        assert_eq!(err.error_type, "cancelled");
        assert_eq!(err.fallback_reason, "cancelled");
        assert!(!err.fallback_allowed);
        assert!(!err.timed_out);
    }

    #[test]
    fn check_cancellation_passes_when_token_absent_or_active() {
        let token = tokio_util::sync::CancellationToken::new();
        assert!(check_cancellation(None).is_ok());
        assert!(check_cancellation(Some(&token)).is_ok());
    }

    #[test]
    fn pre_cancelled_token_short_circuits_memory_search_quickly() {
        use tools_mcp_core::cancellation::CURRENT_CANCEL_TOKEN;

        let root = workspace_test_dir("pre_cancelled_token_short_circuits_memory_search_quickly");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        for idx in 0..32 {
            fs::write(root.join(format!("file_{idx}.txt")), b"needle\n").expect("write fixture");
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        let req = memory_req(&root).normalize();
        let limits = test_limits();
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();

        let started = Instant::now();
        let outcome = runtime.block_on(async {
            CURRENT_CANCEL_TOKEN
                .scope(token.clone(), async {
                    get_or_build_snapshot(
                        &req,
                        &limits,
                        Instant::now() + Duration::from_secs(30),
                        false,
                    )
                })
                .await
        });
        let elapsed = started.elapsed();

        let err = outcome.expect_err("pre-cancelled token must short-circuit");
        assert_eq!(err.error_type, "cancelled");
        assert!(
            elapsed < Duration::from_millis(500),
            "cancellation should be observed quickly; took {elapsed:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_distinct_keys_do_not_serialize_on_per_key_condvar() {
        let root_a = workspace_test_dir("concurrent_distinct_keys_do_not_serialize_a");
        let root_b = workspace_test_dir("concurrent_distinct_keys_do_not_serialize_b");
        let _ = fs::remove_dir_all(&root_a);
        let _ = fs::remove_dir_all(&root_b);
        fs::create_dir_all(&root_a).expect("create root a");
        fs::create_dir_all(&root_b).expect("create root b");
        fs::write(root_a.join("sample.txt"), "needle\n").expect("write fixture a");
        fs::write(root_b.join("sample.txt"), "needle\n").expect("write fixture b");

        let req_a = memory_req(&root_a).normalize();
        let req_b = memory_req(&root_b).normalize();
        let hook_root_a = req_a.root().to_string();
        let hook_root_b = req_b.root().to_string();

        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let entered = Arc::new((Mutex::new(false), Condvar::new()));

        let gate_for_hook = gate.clone();
        let entered_for_hook = entered.clone();
        let hook_root_a_for_build = hook_root_a.clone();
        let build_hook: Arc<IndexBuildTestHook> = Arc::new(move |selector, _generation| {
            if selector.root_arg() != hook_root_a_for_build {
                return;
            }
            {
                let (lock, cvar) = &*entered_for_hook;
                let mut state = lock.lock().expect("entered mutex");
                *state = true;
                cvar.notify_all();
            }
            let (lock, cvar) = &*gate_for_hook;
            let mut released = lock.lock().expect("gate mutex");
            while !*released {
                released = cvar.wait(released).expect("gate condvar");
            }
        });

        let _guard = install_index_build_hooks(Some(build_hook), None);
        let limits = test_limits();

        let req_a_thread = req_a.clone();
        let limits_a = limits.clone();
        let blocked = thread::spawn(move || {
            get_or_build_snapshot(
                &req_a_thread,
                &limits_a,
                Instant::now() + Duration::from_secs(30),
                false,
            )
            .expect("blocked build")
        });

        {
            let (lock, cvar) = &*entered;
            let mut state = lock.lock().expect("entered mutex");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !*state {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(remaining > Duration::ZERO, "first build never entered");
                let (next_state, result) = cvar
                    .wait_timeout(state, remaining)
                    .expect("entered condvar");
                state = next_state;
                if *state {
                    break;
                }
                assert!(!result.timed_out(), "timed out waiting for first build");
            }
        }

        let started = Instant::now();
        let outcome_b = get_or_build_snapshot(
            &req_b,
            &limits,
            Instant::now() + Duration::from_secs(10),
            false,
        )
        .expect("rootB build should complete independently");
        let elapsed = started.elapsed();

        assert_eq!(outcome_b.cache_status, "miss");
        assert_ne!(
            hook_root_a, hook_root_b,
            "distinct keys required for isolation test"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "rootB build should not wait on rootA's blocked build; elapsed={elapsed:?}"
        );

        {
            let (lock, cvar) = &*gate;
            let mut released = lock.lock().expect("gate mutex");
            *released = true;
            cvar.notify_all();
        }
        let _ = blocked.join().expect("blocked thread");

        let _ = fs::remove_dir_all(&root_a);
        let _ = fs::remove_dir_all(&root_b);
    }
}
