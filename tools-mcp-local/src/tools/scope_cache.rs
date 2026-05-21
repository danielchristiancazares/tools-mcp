#![allow(dead_code)]

use ignore::WalkBuilder;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque, hash_map::DefaultHasher};
use std::env;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Instant, SystemTime};

const DEFAULT_REPO_SCOPE_CACHE_MAX_ENTRIES: usize = 32;
const DEFAULT_REPO_SCOPE_CACHE_MAX_FILES_TOTAL: usize = 200_000;
const DEFAULT_REPO_SCOPE_CACHE_FULL_VALIDATE_INTERVAL: u64 = 32;
const DEFAULT_DIR_CACHE_MAX_ENTRIES: usize = 64;
const DEFAULT_OUTLINE_CACHE_MAX_ENTRIES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScopeFileType {
    File,
    Dir,
    Symlink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub change_marker: Option<MetadataChangeMarker>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeEntry {
    pub path: PathBuf,
    pub rendered_path: String,
    pub file_type: ScopeFileType,
    pub basename: String,
    pub metadata_stamp: MetadataStamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataFingerprintEntry {
    pub path: PathBuf,
    pub stamp: MetadataStamp,
}

#[derive(Clone, Debug)]
pub struct RecursiveScopeSnapshot {
    pub root: PathBuf,
    pub entries: Vec<ScopeEntry>,
    pub directories: Vec<MetadataFingerprintEntry>,
    pub ignore_fingerprint: Option<IgnoreFingerprint>,
    pub generation: u64,
    pub built_at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IgnoreFingerprint {
    entries: Vec<IgnoreFingerprintEntry>,
}

impl IgnoreFingerprint {
    pub fn change_reason(&self, current: &Self) -> Option<&'static str> {
        if self.entries == current.entries {
            return None;
        }

        for entry in &self.entries {
            match current
                .entries
                .iter()
                .find(|current| current.path == entry.path)
            {
                Some(current) if current == entry => {}
                Some(_) | None => return Some(entry.reason),
            }
        }

        current
            .entries
            .iter()
            .find(|entry| {
                !self
                    .entries
                    .iter()
                    .any(|expected| expected.path == entry.path)
            })
            .map(|entry| entry.reason)
            .or(Some("ignore_rules_changed"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IgnoreFingerprintEntry {
    path: PathBuf,
    reason: &'static str,
    stamp: Option<IgnoreControlStamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IgnoreControlStamp {
    metadata: MetadataStamp,
    content_hash: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub basename: String,
    pub file_type: ScopeFileType,
    pub metadata_stamp: Option<MetadataStamp>,
}

#[derive(Clone, Debug)]
pub struct DirEntriesSnapshot {
    pub path: PathBuf,
    pub show_hidden: bool,
    pub entries: Vec<DirEntry>,
    pub built_at: Instant,
}

#[derive(Debug)]
pub enum ScopeCacheError {
    Walk(String),
    Timeout,
    Io(io::Error),
}

impl fmt::Display for ScopeCacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Walk(message) => write!(f, "scope walk failed: {message}"),
            Self::Timeout => write!(f, "scope cache operation timed out"),
            Self::Io(err) => write!(f, "scope cache I/O failed: {err}"),
        }
    }
}

impl std::error::Error for ScopeCacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Walk(_) | Self::Timeout => None,
        }
    }
}

impl From<io::Error> for ScopeCacheError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RepoScopeKey {
    pub root: PathBuf,
    pub hidden: bool,
    pub follow: bool,
    pub no_ignore: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RepoScopeCacheLimits {
    max_entries: usize,
    max_files_total: usize,
    full_validate_interval: u64,
}

#[derive(Debug)]
struct RepoScopeCacheEntry {
    snapshot: Arc<RecursiveScopeSnapshot>,
    dirty: bool,
    full_validate_pending: bool,
    queries_since_full_validate: u64,
}

#[derive(Debug)]
struct RepoScopeCacheInner {
    next_generation: u64,
    entries: HashMap<RepoScopeKey, RepoScopeCacheEntry>,
    access_order: VecDeque<RepoScopeKey>,
}

pub struct RepoScopeCache {
    inner: Mutex<RepoScopeCacheInner>,
    limits: RepoScopeCacheLimits,
}

pub fn repo_scope_cache() -> &'static RepoScopeCache {
    static CACHE: OnceLock<RepoScopeCache> = OnceLock::new();
    CACHE.get_or_init(RepoScopeCache::from_env)
}

impl RepoScopeCache {
    pub fn get_or_build(
        &self,
        key: &RepoScopeKey,
        deadline: Instant,
    ) -> Result<Arc<RecursiveScopeSnapshot>, ScopeCacheError> {
        check_deadline(deadline)?;
        if let Some(snapshot) = self.cached_snapshot(key) {
            return self.rebuild_if_stale(key, &snapshot, deadline);
        }

        let generation = self.reserve_generation();
        let snapshot = Arc::new(build_recursive_scope_snapshot(key, generation, deadline)?);
        Ok(self.store_snapshot(key.clone(), snapshot))
    }

    pub fn invalidate(&self, key: &RepoScopeKey) {
        let mut inner = lock_or_recover(&self.inner);
        inner.entries.remove(key);
        inner.access_order.retain(|existing| existing != key);
    }

    fn rebuild_if_stale(
        &self,
        key: &RepoScopeKey,
        snapshot: &Arc<RecursiveScopeSnapshot>,
        deadline: Instant,
    ) -> Result<Arc<RecursiveScopeSnapshot>, ScopeCacheError> {
        check_deadline(deadline)?;

        let Some((current_snapshot, dirty, full_validate_pending)) = self.validation_state(key)
        else {
            let generation = self.reserve_generation();
            let rebuilt = Arc::new(build_recursive_scope_snapshot(key, generation, deadline)?);
            return Ok(self.store_snapshot(key.clone(), rebuilt));
        };

        if !Arc::ptr_eq(&current_snapshot, snapshot) {
            return Ok(current_snapshot);
        }

        if dirty {
            return self.rebuild_and_store(key, deadline);
        }

        if !directory_fingerprints_match(&current_snapshot.directories, deadline)? {
            self.mark_dirty(key, snapshot);
            return self.rebuild_and_store(key, deadline);
        }

        if ignore_fingerprint_change_reason(
            &current_snapshot.root,
            current_snapshot
                .directories
                .iter()
                .map(|entry| entry.path.clone()),
            key.no_ignore,
            current_snapshot.ignore_fingerprint.as_ref(),
            deadline,
        )?
        .is_some()
        {
            self.mark_dirty(key, snapshot);
            return self.rebuild_and_store(key, deadline);
        }

        if full_validate_pending {
            let generation = self.reserve_generation();
            let candidate = Arc::new(build_recursive_scope_snapshot(key, generation, deadline)?);
            if recursive_scope_snapshot_matches(&current_snapshot, &candidate) {
                self.clear_validation_flags(key, snapshot);
                return Ok(current_snapshot);
            }
            return Ok(self.store_snapshot(key.clone(), candidate));
        }

        Ok(current_snapshot)
    }

    fn from_env() -> Self {
        Self::new(RepoScopeCacheLimits {
            max_entries: read_env_usize(
                "TOOLS_SCOPE_CACHE_MAX_ENTRIES",
                DEFAULT_REPO_SCOPE_CACHE_MAX_ENTRIES,
            ),
            max_files_total: read_env_usize(
                "TOOLS_SCOPE_CACHE_MAX_FILES_TOTAL",
                DEFAULT_REPO_SCOPE_CACHE_MAX_FILES_TOTAL,
            ),
            full_validate_interval: read_env_u64(
                "TOOLS_SCOPE_CACHE_FULL_VALIDATE_INTERVAL",
                DEFAULT_REPO_SCOPE_CACHE_FULL_VALIDATE_INTERVAL,
            ),
        })
    }

    fn new(limits: RepoScopeCacheLimits) -> Self {
        Self {
            inner: Mutex::new(RepoScopeCacheInner {
                next_generation: 1,
                entries: HashMap::new(),
                access_order: VecDeque::new(),
            }),
            limits,
        }
    }

    fn cached_snapshot(&self, key: &RepoScopeKey) -> Option<Arc<RecursiveScopeSnapshot>> {
        let mut inner = lock_or_recover(&self.inner);
        let snapshot = {
            let entry = inner.entries.get_mut(key)?;
            entry.queries_since_full_validate = entry.queries_since_full_validate.saturating_add(1);
            if self.limits.full_validate_interval == 0
                || entry.queries_since_full_validate >= self.limits.full_validate_interval
            {
                entry.queries_since_full_validate = 0;
                entry.full_validate_pending = true;
            }
            entry.snapshot.clone()
        };
        inner.touch(key);
        Some(snapshot)
    }

    fn validation_state(
        &self,
        key: &RepoScopeKey,
    ) -> Option<(Arc<RecursiveScopeSnapshot>, bool, bool)> {
        let inner = lock_or_recover(&self.inner);
        inner.entries.get(key).map(|entry| {
            (
                entry.snapshot.clone(),
                entry.dirty,
                entry.full_validate_pending,
            )
        })
    }

    fn reserve_generation(&self) -> u64 {
        let mut inner = lock_or_recover(&self.inner);
        let generation = inner.next_generation;
        inner.next_generation = inner.next_generation.saturating_add(1);
        generation
    }

    fn rebuild_and_store(
        &self,
        key: &RepoScopeKey,
        deadline: Instant,
    ) -> Result<Arc<RecursiveScopeSnapshot>, ScopeCacheError> {
        let generation = self.reserve_generation();
        let rebuilt = Arc::new(build_recursive_scope_snapshot(key, generation, deadline)?);
        Ok(self.store_snapshot(key.clone(), rebuilt))
    }

    fn store_snapshot(
        &self,
        key: RepoScopeKey,
        snapshot: Arc<RecursiveScopeSnapshot>,
    ) -> Arc<RecursiveScopeSnapshot> {
        let mut inner = lock_or_recover(&self.inner);
        inner.entries.insert(
            key.clone(),
            RepoScopeCacheEntry {
                snapshot: snapshot.clone(),
                dirty: false,
                full_validate_pending: false,
                queries_since_full_validate: 0,
            },
        );
        inner.touch(&key);
        inner.evict_to_capacity(self.limits);
        snapshot
    }

    fn mark_dirty(&self, key: &RepoScopeKey, snapshot: &Arc<RecursiveScopeSnapshot>) {
        let mut inner = lock_or_recover(&self.inner);
        if let Some(entry) = inner.entries.get_mut(key)
            && Arc::ptr_eq(&entry.snapshot, snapshot)
        {
            entry.dirty = true;
        }
    }

    fn clear_validation_flags(&self, key: &RepoScopeKey, snapshot: &Arc<RecursiveScopeSnapshot>) {
        let mut inner = lock_or_recover(&self.inner);
        if let Some(entry) = inner.entries.get_mut(key)
            && Arc::ptr_eq(&entry.snapshot, snapshot)
        {
            entry.dirty = false;
            entry.full_validate_pending = false;
        }
    }
}

impl RepoScopeCacheInner {
    fn touch(&mut self, key: &RepoScopeKey) {
        self.access_order.retain(|existing| existing != key);
        self.access_order.push_back(key.clone());
    }

    fn total_cached_scope_entries(&self) -> usize {
        self.entries
            .values()
            .map(|entry| entry.snapshot.entries.len())
            .sum()
    }

    fn evict_to_capacity(&mut self, limits: RepoScopeCacheLimits) {
        while self.entries.len() > limits.max_entries
            || self.total_cached_scope_entries() > limits.max_files_total
        {
            let Some(oldest) = self.access_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DirEntriesKey {
    pub path: PathBuf,
    pub show_hidden: bool,
}

#[derive(Debug)]
struct DirEntriesCacheEntry {
    snapshot: Arc<DirEntriesSnapshot>,
    directory_modified: Option<SystemTime>,
}

#[derive(Debug)]
struct DirEntriesCacheInner {
    entries: HashMap<DirEntriesKey, DirEntriesCacheEntry>,
    access_order: VecDeque<DirEntriesKey>,
}

pub struct DirEntriesCache {
    inner: Mutex<DirEntriesCacheInner>,
    max_entries: usize,
}

pub fn dir_entries_cache() -> &'static DirEntriesCache {
    static CACHE: OnceLock<DirEntriesCache> = OnceLock::new();
    CACHE.get_or_init(DirEntriesCache::from_env)
}

impl DirEntriesCache {
    pub async fn get_or_build(
        &self,
        key: &DirEntriesKey,
    ) -> Result<Arc<DirEntriesSnapshot>, ScopeCacheError> {
        if let Some((snapshot, cached_modified)) = self.cached_snapshot(key)
            && let Ok(current_modified) = directory_modified_async(&key.path).await
            && current_modified == cached_modified
        {
            return Ok(snapshot);
        }

        let (snapshot, directory_modified) = build_dir_entries_snapshot(key).await?;
        let snapshot = Arc::new(snapshot);
        Ok(self.store_snapshot(key.clone(), snapshot, directory_modified))
    }

    fn from_env() -> Self {
        Self::new(read_env_usize(
            "TOOLS_DIR_CACHE_MAX_ENTRIES",
            DEFAULT_DIR_CACHE_MAX_ENTRIES,
        ))
    }

    fn new(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(DirEntriesCacheInner {
                entries: HashMap::new(),
                access_order: VecDeque::new(),
            }),
            max_entries,
        }
    }

    fn cached_snapshot(
        &self,
        key: &DirEntriesKey,
    ) -> Option<(Arc<DirEntriesSnapshot>, Option<SystemTime>)> {
        let mut inner = lock_or_recover(&self.inner);
        let (snapshot, directory_modified) = {
            let entry = inner.entries.get(key)?;
            (entry.snapshot.clone(), entry.directory_modified)
        };
        inner.touch(key);
        Some((snapshot, directory_modified))
    }

    fn store_snapshot(
        &self,
        key: DirEntriesKey,
        snapshot: Arc<DirEntriesSnapshot>,
        directory_modified: Option<SystemTime>,
    ) -> Arc<DirEntriesSnapshot> {
        let mut inner = lock_or_recover(&self.inner);
        inner.entries.insert(
            key.clone(),
            DirEntriesCacheEntry {
                snapshot: snapshot.clone(),
                directory_modified,
            },
        );
        inner.touch(&key);
        inner.evict_to_capacity(self.max_entries);
        snapshot
    }
}

impl DirEntriesCacheInner {
    fn touch(&mut self, key: &DirEntriesKey) {
        self.access_order.retain(|existing| existing != key);
        self.access_order.push_back(key.clone());
    }

    fn evict_to_capacity(&mut self, max_entries: usize) {
        while self.entries.len() > max_entries {
            let Some(oldest) = self.access_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OutlineKey {
    pub path: PathBuf,
    pub language: String,
    pub modified: Option<SystemTime>,
    pub len: u64,
    pub content_hash: u64,
}

#[derive(Debug)]
struct OutlineAstCacheInner {
    entries: HashMap<OutlineKey, Arc<String>>,
    access_order: VecDeque<OutlineKey>,
}

pub struct OutlineAstCache {
    inner: Mutex<OutlineAstCacheInner>,
    max_entries: usize,
}

pub fn outline_ast_cache() -> &'static OutlineAstCache {
    static CACHE: OnceLock<OutlineAstCache> = OnceLock::new();
    CACHE.get_or_init(OutlineAstCache::from_env)
}

impl OutlineAstCache {
    pub fn get(&self, key: &OutlineKey) -> Option<Arc<String>> {
        let mut inner = lock_or_recover(&self.inner);
        let rendered = inner.entries.get(key)?.clone();
        inner.touch(key);
        Some(rendered)
    }

    pub fn insert(&self, key: OutlineKey, rendered: Arc<String>) {
        let mut inner = lock_or_recover(&self.inner);
        inner.entries.insert(key.clone(), rendered);
        inner.touch(&key);
        inner.evict_to_capacity(self.max_entries);
    }

    fn from_env() -> Self {
        Self::new(read_env_usize(
            "TOOLS_OUTLINE_CACHE_MAX_ENTRIES",
            DEFAULT_OUTLINE_CACHE_MAX_ENTRIES,
        ))
    }

    fn new(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(OutlineAstCacheInner {
                entries: HashMap::new(),
                access_order: VecDeque::new(),
            }),
            max_entries,
        }
    }
}

impl OutlineAstCacheInner {
    fn touch(&mut self, key: &OutlineKey) {
        self.access_order.retain(|existing| existing != key);
        self.access_order.push_back(key.clone());
    }

    fn evict_to_capacity(&mut self, max_entries: usize) {
        while self.entries.len() > max_entries {
            let Some(oldest) = self.access_order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

#[cfg(unix)]
pub type MetadataChangeMarker = UnixMetadataChangeMarker;

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnixMetadataChangeMarker {
    dev: u64,
    ino: u64,
    mode: u32,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(windows)]
pub type MetadataChangeMarker = WindowsMetadataChangeMarker;

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowsMetadataChangeMarker {
    creation_time: u64,
    last_write_time: u64,
    file_attributes: u32,
}

#[cfg(not(any(unix, windows)))]
pub type MetadataChangeMarker = ();

fn build_recursive_scope_snapshot(
    key: &RepoScopeKey,
    generation: u64,
    deadline: Instant,
) -> Result<RecursiveScopeSnapshot, ScopeCacheError> {
    check_deadline(deadline)?;

    let mut builder = WalkBuilder::new(&key.root);
    // Mirror the file selection walker semantics in search_file_selection.rs.
    builder
        .hidden(!key.hidden)
        .follow_links(key.follow)
        .ignore(!key.no_ignore)
        .git_ignore(!key.no_ignore)
        .git_global(!key.no_ignore)
        .git_exclude(!key.no_ignore);

    let mut entries = Vec::new();
    let mut directory_paths = BTreeSet::new();
    directory_paths.insert(key.root.clone());

    for walked in builder.build() {
        check_deadline(deadline)?;
        let entry = walked.map_err(|err| ScopeCacheError::Walk(err.to_string()))?;
        let path = entry.into_path();

        if path == key.root {
            continue;
        }

        if let Some(parent) = path.parent() {
            directory_paths.insert(parent.to_path_buf());
        }

        let metadata = fs::symlink_metadata(&path)?;
        let file_type = scope_file_type_from_file_type(&metadata.file_type());
        if matches!(file_type, ScopeFileType::Dir) {
            directory_paths.insert(path.clone());
        }

        let rendered_path = rendered_relative_path(&key.root, &path);
        let basename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| rendered_path.clone());

        entries.push(ScopeEntry {
            path,
            rendered_path,
            file_type,
            basename,
            metadata_stamp: metadata_stamp_from_metadata(&metadata),
        });
    }

    entries.sort_by(|left, right| {
        left.rendered_path
            .cmp(&right.rendered_path)
            .then_with(|| left.path.cmp(&right.path))
    });

    let directories = directory_paths
        .into_iter()
        .map(|path| {
            check_deadline(deadline)?;
            let metadata = fs::metadata(&path)?;
            Ok(MetadataFingerprintEntry {
                path,
                stamp: metadata_stamp_from_metadata(&metadata),
            })
        })
        .collect::<Result<Vec<_>, ScopeCacheError>>()?;
    let ignore_fingerprint = build_ignore_fingerprint(
        &key.root,
        directories.iter().map(|entry| entry.path.clone()),
        key.no_ignore,
        deadline,
    )?;

    Ok(RecursiveScopeSnapshot {
        root: key.root.clone(),
        entries,
        directories,
        ignore_fingerprint,
        generation,
        built_at: Instant::now(),
    })
}

async fn build_dir_entries_snapshot(
    key: &DirEntriesKey,
) -> Result<(DirEntriesSnapshot, Option<SystemTime>), ScopeCacheError> {
    let mut read_dir = tokio::fs::read_dir(&key.path).await?;
    let mut entries = Vec::new();

    while let Some(entry) = read_dir.next_entry().await? {
        let basename = entry.file_name().to_string_lossy().into_owned();
        if !key.show_hidden && basename.starts_with('.') {
            continue;
        }

        let file_type = entry.file_type().await?;
        let metadata_stamp = tokio::fs::symlink_metadata(entry.path())
            .await
            .ok()
            .map(|metadata| metadata_stamp_from_metadata(&metadata));

        entries.push(DirEntry {
            basename,
            file_type: scope_file_type_from_file_type(&file_type),
            metadata_stamp,
        });
    }

    entries.sort_by(|left, right| left.basename.cmp(&right.basename));
    let directory_modified = directory_modified_async(&key.path).await?;

    Ok((
        DirEntriesSnapshot {
            path: key.path.clone(),
            show_hidden: key.show_hidden,
            entries,
            built_at: Instant::now(),
        },
        directory_modified,
    ))
}

async fn directory_modified_async(path: &Path) -> io::Result<Option<SystemTime>> {
    Ok(tokio::fs::metadata(path).await?.modified().ok())
}

fn directory_fingerprints_match(
    directories: &[MetadataFingerprintEntry],
    deadline: Instant,
) -> Result<bool, ScopeCacheError> {
    for entry in directories {
        check_deadline(deadline)?;
        let metadata = match fs::metadata(&entry.path) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(false),
        };
        if !metadata_stamp_matches(&entry.stamp, &metadata) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn ignore_fingerprint_change_reason<I>(
    root: &Path,
    directories: I,
    no_ignore: bool,
    expected: Option<&IgnoreFingerprint>,
    deadline: Instant,
) -> Result<Option<&'static str>, ScopeCacheError>
where
    I: IntoIterator<Item = PathBuf>,
{
    let current = build_ignore_fingerprint(root, directories, no_ignore, deadline)?;
    Ok(match (expected, current.as_ref()) {
        (None, None) => None,
        (Some(expected), Some(current)) => expected.change_reason(current),
        (Some(_), None) | (None, Some(_)) => Some("ignore_rules_changed"),
    })
}

pub fn build_ignore_fingerprint<I>(
    root: &Path,
    directories: I,
    no_ignore: bool,
    deadline: Instant,
) -> Result<Option<IgnoreFingerprint>, ScopeCacheError>
where
    I: IntoIterator<Item = PathBuf>,
{
    if no_ignore {
        return Ok(None);
    }

    let mut controls = BTreeMap::<PathBuf, &'static str>::new();
    for directory in directories {
        check_deadline(deadline)?;
        controls
            .entry(directory.join(".ignore"))
            .or_insert("ignore_file_changed");
        controls
            .entry(directory.join(".gitignore"))
            .or_insert("gitignore_changed");
        controls
            .entry(directory.join(".git").join("info").join("exclude"))
            .or_insert("git_exclude_changed");
    }
    if let Some(path) = ignore::gitignore::gitconfig_excludes_path() {
        controls.entry(path).or_insert("global_ignore_changed");
    }
    if controls.is_empty() {
        controls
            .entry(root.join(".ignore"))
            .or_insert("ignore_file_changed");
        controls
            .entry(root.join(".gitignore"))
            .or_insert("gitignore_changed");
        controls
            .entry(root.join(".git").join("info").join("exclude"))
            .or_insert("git_exclude_changed");
    }

    let mut entries = Vec::with_capacity(controls.len());
    for (path, reason) in controls {
        check_deadline(deadline)?;
        entries.push(IgnoreFingerprintEntry {
            stamp: ignore_control_stamp(&path, deadline)?,
            path,
            reason,
        });
    }

    Ok(Some(IgnoreFingerprint { entries }))
}

fn ignore_control_stamp(
    path: &Path,
    deadline: Instant,
) -> Result<Option<IgnoreControlStamp>, ScopeCacheError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            check_deadline(deadline)?;
            let content = fs::read(path)?;
            check_deadline(deadline)?;
            Ok(Some(IgnoreControlStamp {
                metadata: metadata_stamp_from_metadata(&metadata),
                content_hash: content_hash(&content),
            }))
        }
        Ok(_) => Ok(None),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(ScopeCacheError::Io(err)),
    }
}

fn content_hash(content: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn recursive_scope_snapshot_matches(
    current: &RecursiveScopeSnapshot,
    candidate: &RecursiveScopeSnapshot,
) -> bool {
    current.root == candidate.root
        && current.entries == candidate.entries
        && current.directories == candidate.directories
        && current.ignore_fingerprint == candidate.ignore_fingerprint
}

fn metadata_stamp_from_metadata(metadata: &fs::Metadata) -> MetadataStamp {
    MetadataStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        change_marker: metadata_change_marker(metadata),
    }
}

fn metadata_stamp_matches(expected: &MetadataStamp, metadata: &fs::Metadata) -> bool {
    *expected == metadata_stamp_from_metadata(metadata)
}

fn scope_file_type_from_file_type(file_type: &fs::FileType) -> ScopeFileType {
    if file_type.is_symlink() {
        ScopeFileType::Symlink
    } else if file_type.is_dir() {
        ScopeFileType::Dir
    } else {
        ScopeFileType::File
    }
}

fn rendered_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .filter(|stripped| !stripped.as_os_str().is_empty())
        .map_or_else(
            || path.to_string_lossy().into_owned(),
            |stripped| stripped.to_string_lossy().into_owned(),
        )
}

fn read_env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn read_env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn check_deadline(deadline: Instant) -> Result<(), ScopeCacheError> {
    if Instant::now() >= deadline {
        Err(ScopeCacheError::Timeout)
    } else {
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::Duration;

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let base = std::env::current_dir()
                .expect("current directory")
                .join("target")
                .join("scope-cache-tests");
            fs::create_dir_all(&base).expect("create test base directory");
            let unique = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).expect("create test directory");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    fn write_file(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write test file");
    }

    fn create_file_until_directory_mtime_changes(dir: &Path, prefix: &str) {
        let before = fs::metadata(dir)
            .expect("directory metadata")
            .modified()
            .ok();

        for attempt in 0..32 {
            let path = dir.join(format!(".{prefix}-{attempt}.txt"));
            fs::write(&path, b"bump").expect("bump directory mtime");
            let after = fs::metadata(dir)
                .expect("directory metadata after bump")
                .modified()
                .ok();
            if after != before {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }

        panic!(
            "directory modified time did not change for {}",
            dir.display()
        );
    }

    fn repo_key(root: &Path) -> RepoScopeKey {
        RepoScopeKey {
            root: root.to_path_buf(),
            hidden: false,
            follow: false,
            no_ignore: false,
        }
    }

    #[test]
    fn repo_scope_cache_returns_same_arc_until_directory_changes() {
        let dir = TestDir::new("repo-snapshot-hit");
        write_file(&dir.path().join("alpha.txt"), "alpha");

        let cache = RepoScopeCache::new(RepoScopeCacheLimits {
            max_entries: 8,
            max_files_total: 1_000,
            full_validate_interval: 32,
        });
        let key = repo_key(dir.path());

        let first = cache
            .get_or_build(&key, deadline())
            .expect("initial snapshot");
        let second = cache
            .get_or_build(&key, deadline())
            .expect("cached snapshot");
        assert!(Arc::ptr_eq(&first, &second));

        create_file_until_directory_mtime_changes(dir.path(), "repo-refresh");

        let third = cache
            .get_or_build(&key, deadline())
            .expect("rebuilt snapshot after directory change");
        assert!(!Arc::ptr_eq(&second, &third));
    }

    #[test]
    fn repo_scope_cache_rebuilds_when_gitignore_contents_change() {
        let dir = TestDir::new("repo-snapshot-gitignore-change");
        write_file(&dir.path().join("alpha.txt"), "alpha");
        write_file(&dir.path().join(".gitignore"), "# before\n");

        let cache = RepoScopeCache::new(RepoScopeCacheLimits {
            max_entries: 8,
            max_files_total: 1_000,
            full_validate_interval: 32,
        });
        let key = repo_key(dir.path());

        let first = cache
            .get_or_build(&key, deadline())
            .expect("initial snapshot");
        write_file(&dir.path().join(".gitignore"), "# after\n");
        let second = cache
            .get_or_build(&key, deadline())
            .expect("rebuilt snapshot after gitignore mutation");

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(first.entries, second.entries);
        assert_ne!(first.ignore_fingerprint, second.ignore_fingerprint);
    }

    #[test]
    fn repo_scope_cache_evicts_least_recently_used_entry() {
        let dir = TestDir::new("repo-snapshot-lru");
        let first_root = dir.path().join("first");
        let second_root = dir.path().join("second");
        let third_root = dir.path().join("third");
        fs::create_dir_all(&first_root).expect("create first root");
        fs::create_dir_all(&second_root).expect("create second root");
        fs::create_dir_all(&third_root).expect("create third root");

        let cache = RepoScopeCache::new(RepoScopeCacheLimits {
            max_entries: 2,
            max_files_total: 1_000,
            full_validate_interval: 32,
        });
        let first_key = repo_key(&first_root);
        let second_key = repo_key(&second_root);
        let third_key = repo_key(&third_root);

        cache
            .get_or_build(&first_key, deadline())
            .expect("cache first root");
        cache
            .get_or_build(&second_key, deadline())
            .expect("cache second root");
        cache
            .get_or_build(&first_key, deadline())
            .expect("promote first root");
        cache
            .get_or_build(&third_key, deadline())
            .expect("cache third root");

        let inner = lock_or_recover(&cache.inner);
        assert!(inner.entries.contains_key(&first_key));
        assert!(!inner.entries.contains_key(&second_key));
        assert!(inner.entries.contains_key(&third_key));
    }

    #[tokio::test]
    async fn dir_entries_cache_rebuilds_after_directory_change() {
        let dir = TestDir::new("dir-entries-hit");
        write_file(&dir.path().join("visible.txt"), "visible");

        let cache = DirEntriesCache::new(8);
        let key = DirEntriesKey {
            path: dir.path().to_path_buf(),
            show_hidden: false,
        };

        let first = cache
            .get_or_build(&key)
            .await
            .expect("initial dir snapshot");
        let second = cache.get_or_build(&key).await.expect("cached dir snapshot");
        assert!(Arc::ptr_eq(&first, &second));

        create_file_until_directory_mtime_changes(dir.path(), "dir-refresh");

        let third = cache
            .get_or_build(&key)
            .await
            .expect("rebuilt dir snapshot after directory change");
        assert!(!Arc::ptr_eq(&second, &third));
    }

    #[test]
    fn outline_ast_cache_hits_and_misses_by_exact_key() {
        let cache = OutlineAstCache::new(8);
        let key = OutlineKey {
            path: PathBuf::from("outline.rs"),
            language: "rust".to_string(),
            modified: Some(SystemTime::UNIX_EPOCH),
            len: 42,
            content_hash: 7,
        };
        let rendered = Arc::new("fn main()".to_string());

        cache.insert(key.clone(), rendered.clone());

        let hit = cache.get(&key).expect("outline cache hit");
        assert!(Arc::ptr_eq(&hit, &rendered));

        let mismatch = OutlineKey {
            len: key.len + 1,
            ..key.clone()
        };
        assert!(cache.get(&mismatch).is_none());
    }

    #[test]
    fn outline_ast_cache_evicts_oldest_entry() {
        let cache = OutlineAstCache::new(2);
        let first = OutlineKey {
            path: PathBuf::from("first.rs"),
            language: "rust".to_string(),
            modified: Some(SystemTime::UNIX_EPOCH),
            len: 1,
            content_hash: 1,
        };
        let second = OutlineKey {
            path: PathBuf::from("second.rs"),
            language: "rust".to_string(),
            modified: Some(SystemTime::UNIX_EPOCH),
            len: 2,
            content_hash: 2,
        };
        let third = OutlineKey {
            path: PathBuf::from("third.rs"),
            language: "rust".to_string(),
            modified: Some(SystemTime::UNIX_EPOCH),
            len: 3,
            content_hash: 3,
        };

        cache.insert(first.clone(), Arc::new("first".to_string()));
        cache.insert(second.clone(), Arc::new("second".to_string()));
        assert!(cache.get(&first).is_some());
        cache.insert(third.clone(), Arc::new("third".to_string()));

        assert!(cache.get(&first).is_some());
        assert!(cache.get(&second).is_none());
        assert!(cache.get(&third).is_some());
    }
}
