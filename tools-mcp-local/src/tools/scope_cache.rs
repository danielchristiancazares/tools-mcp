#![allow(dead_code)]

use ignore::WalkBuilder;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque, hash_map::DefaultHasher};
use std::env;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Racy-timestamp slack shared by every stamp-trust rule in the local tools,
/// mirroring git's racy-index handling: a stamp only proves "nothing changed"
/// when the recorded mtime is at least this much older than the observation
/// that recorded it, so a write landing in the same filesystem timestamp
/// granule can never hide behind an unchanged stamp.
pub(crate) const SCOPE_STAMP_RACY_SLACK: Duration = Duration::from_secs(2);

const DEFAULT_REPO_SCOPE_CACHE_MAX_ENTRIES: usize = 32;
const DEFAULT_REPO_SCOPE_CACHE_MAX_FILES_TOTAL: usize = 200_000;
const DEFAULT_REPO_SCOPE_CACHE_FULL_VALIDATE_INTERVAL: u64 = 32;
const DEFAULT_DIR_CACHE_MAX_ENTRIES: usize = 64;
const DEFAULT_OUTLINE_CACHE_MAX_ENTRIES: usize = 256;
const IGNORE_HASH_BUFFER_BYTES: usize = 8 * 1024;

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
    /// Wall-clock instant captured before the build's walk started. Every
    /// stamp in the snapshot was observed at or after this floor, so a
    /// directory stamp whose mtime is at least [`SCOPE_STAMP_RACY_SLACK`]
    /// older than it is race-free: any later change to the directory's
    /// direct-child membership must produce a different stamp.
    pub stamp_observation_floor: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IgnoreFingerprint {
    entries: Vec<IgnoreFingerprintEntry>,
    /// Scope directories that contained a `.git` directory when the
    /// fingerprint was built. A recorded-absent `.git/info/exclude` control
    /// in a directory outside this set can only start existing if a `.git`
    /// directory appears first, which bumps the scope directory's own stamp;
    /// inside this set the exclude file can appear without touching any
    /// stamped directory, so it must keep being probed.
    git_dirs: BTreeSet<PathBuf>,
}

impl IgnoreFingerprint {
    pub fn change_reason(&self, current: &Self) -> Option<&'static str> {
        if self == current {
            return None;
        }

        let mut expected = self.entries.iter().peekable();
        let mut observed = current.entries.iter().peekable();
        loop {
            match (expected.peek(), observed.peek()) {
                (Some(left), Some(right)) => match left.path.cmp(&right.path) {
                    std::cmp::Ordering::Equal => {
                        if left != right {
                            return Some(left.reason);
                        }
                        expected.next();
                        observed.next();
                    }
                    std::cmp::Ordering::Less => return Some(left.reason),
                    std::cmp::Ordering::Greater => return Some(right.reason),
                },
                (Some(left), None) => return Some(left.reason),
                (None, Some(right)) => return Some(right.reason),
                (None, None) => return Some("ignore_rules_changed"),
            }
        }
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
    pub max_depth: Option<usize>,
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

        // Directory stamps were just re-verified, so recorded-absent controls
        // under race-free directories can skip their existence probes.
        let umbrella_eligible_dirs = umbrella_eligible_directories(&current_snapshot);
        if ignore_fingerprint_change_reason(
            &current_snapshot.root,
            current_snapshot
                .directories
                .iter()
                .map(|entry| entry.path.as_path()),
            key.no_ignore,
            current_snapshot.ignore_fingerprint.as_ref(),
            AbsentControlSkip::VerifiedDirs(&umbrella_eligible_dirs),
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
        let mut total_cached_scope_entries = self.total_cached_scope_entries();
        while self.entries.len() > limits.max_entries
            || total_cached_scope_entries > limits.max_files_total
        {
            let Some(oldest) = self.access_order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                total_cached_scope_entries =
                    total_cached_scope_entries.saturating_sub(removed.snapshot.entries.len());
            }
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
    /// NTFS ChangeTime and volume/file identity, present only when the stamp
    /// was captured through an open handle. Stat-built markers carry `None`.
    handle_info: Option<WindowsHandleChangeInfo>,
}

#[cfg(windows)]
impl WindowsMetadataChangeMarker {
    /// Fields observable from a plain (handle-free) metadata query.
    fn stat_fields(&self) -> (u64, u64, u32) {
        (
            self.creation_time,
            self.last_write_time,
            self.file_attributes,
        )
    }
}

/// By-handle change information equivalent to the Unix `ctime`+`dev`/`ino`
/// marker: `change_time` is NTFS ChangeTime, which the kernel bumps on every
/// data or metadata write — including `SetFileTime` calls that restore
/// `last_write_time` — and which callers cannot set directly. Filesystems
/// that cannot report it yield `None`, keeping byte/hash comparison
/// authoritative. Filesystems that merely synthesize ChangeTime from the
/// write time (e.g. exFAT) cannot distinguish an mtime-restoring rewrite
/// from no change; NTFS is not affected.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowsHandleChangeInfo {
    change_time: i64,
    volume_serial: u64,
    file_id: u128,
}

/// Upgrades `stamp` with by-handle change information when the platform
/// supports it. Must be called before the handle's content is read so a write
/// racing the read leaves the stamp observably stale rather than silently
/// current.
#[cfg(windows)]
fn attach_handle_change_info_to_stamp(stamp: &mut MetadataStamp, file: &fs::File) {
    if let Some(marker) = stamp.change_marker.as_mut() {
        marker.handle_info = windows_handle_change_info(file);
    }
}

#[cfg(not(windows))]
fn attach_handle_change_info_to_stamp(_stamp: &mut MetadataStamp, _file: &fs::File) {}

/// Minimum age a ChangeTime must have before it is trusted as a change
/// marker. NTFS updates timestamps with coarse timer resolution (~16 ms), so
/// a write landing in the same tick as the recorded ChangeTime would be
/// invisible to a pure equality check — the same racy-timestamp rule as
/// [`SCOPE_STAMP_RACY_SLACK`] and git's racy-index handling. Too-fresh
/// observations return `None`, keeping callers on the byte/hash path.
#[cfg(windows)]
const WINDOWS_CHANGE_TIME_RACY_GUARD_TICKS: i64 = 100 * 10_000; // 100 ms in 100 ns ticks

/// Opens `path` with attribute-only access (`FILE_READ_ATTRIBUTES`), which is
/// all `GetFileInformationByHandleEx` metadata queries need. Unlike a
/// `GENERIC_READ` open, this does not trigger antivirus on-access content
/// scanning and cannot conflict with a writer holding the file exclusively,
/// so change-marker re-observation stays cheap and succeeds in strictly more
/// cases. Directories still fail to open (no `FILE_FLAG_BACKUP_SEMANTICS`),
/// matching `File::open` behavior.
#[cfg(windows)]
pub(crate) fn open_for_attributes(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
}

/// Queries ChangeTime plus volume/file identity from an open handle.
/// Returns `None` when the filesystem cannot answer or the ChangeTime is too
/// recent to be race-free.
#[cfg(windows)]
pub(crate) fn windows_handle_change_info(file: &fs::File) -> Option<WindowsHandleChangeInfo> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FILE_ID_INFO, FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx,
    };

    let handle = file.as_raw_handle() as HANDLE;

    let mut basic = MaybeUninit::<FILE_BASIC_INFO>::zeroed();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            u32::try_from(size_of::<FILE_BASIC_INFO>()).expect("FILE_BASIC_INFO fits in u32"),
        )
    };
    if ok == 0 {
        return None;
    }
    let basic = unsafe { basic.assume_init() };
    if basic.ChangeTime == 0 {
        return None;
    }
    if let Some(now_ticks) = windows_filetime_now_ticks()
        && basic.ChangeTime > now_ticks.saturating_sub(WINDOWS_CHANGE_TIME_RACY_GUARD_TICKS)
    {
        return None;
    }

    let mut id = MaybeUninit::<FILE_ID_INFO>::zeroed();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            id.as_mut_ptr().cast(),
            u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO fits in u32"),
        )
    };
    if ok == 0 {
        return None;
    }
    let id = unsafe { id.assume_init() };

    Some(WindowsHandleChangeInfo {
        change_time: basic.ChangeTime,
        volume_serial: id.VolumeSerialNumber,
        file_id: u128::from_le_bytes(id.FileId.Identifier),
    })
}

/// Current system time in FILETIME ticks (100 ns since 1601-01-01), or `None`
/// if the clock reads before the Unix epoch.
#[cfg(windows)]
fn windows_filetime_now_ticks() -> Option<i64> {
    const UNIX_EPOCH_AS_FILETIME_SECONDS: u64 = 11_644_473_600;
    const TICKS_PER_SECOND: u64 = 10_000_000;

    let since_unix_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    let seconds = since_unix_epoch
        .as_secs()
        .checked_add(UNIX_EPOCH_AS_FILETIME_SECONDS)?;
    let ticks = seconds
        .checked_mul(TICKS_PER_SECOND)?
        .checked_add(u64::from(since_unix_epoch.subsec_nanos()) / 100)?;
    i64::try_from(ticks).ok()
}

#[cfg(not(any(unix, windows)))]
pub type MetadataChangeMarker = ();

/// Directories whose recorded stamps are race-free relative to the
/// snapshot's observation floor: their direct-child membership provably
/// cannot change without producing a different stamp, and the build's own
/// probes already saw anything created inside the stamp's timestamp granule.
fn umbrella_eligible_directories(snapshot: &RecursiveScopeSnapshot) -> BTreeSet<PathBuf> {
    snapshot
        .directories
        .iter()
        .filter(|entry| {
            entry.stamp.modified.is_some_and(|modified| {
                modified
                    .checked_add(SCOPE_STAMP_RACY_SLACK)
                    .is_some_and(|threshold| threshold <= snapshot.stamp_observation_floor)
            })
        })
        .map(|entry| entry.path.clone())
        .collect()
}

fn build_recursive_scope_snapshot(
    key: &RepoScopeKey,
    generation: u64,
    deadline: Instant,
) -> Result<RecursiveScopeSnapshot, ScopeCacheError> {
    check_deadline(deadline)?;
    let stamp_observation_floor = SystemTime::now();

    let mut builder = WalkBuilder::new(&key.root);
    // Mirror the file selection walker semantics in search_file_selection.rs.
    builder
        .hidden(!key.hidden)
        .follow_links(key.follow)
        .max_depth(key.max_depth)
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
        // The walker's cached metadata matches `symlink_metadata` semantics
        // when links are not followed (and is free on Windows, where it comes
        // from the directory enumeration). Followed links keep the explicit
        // no-follow stat so symlink entries are stamped as links, as before.
        let walker_metadata = if key.follow {
            None
        } else {
            Some(entry.metadata())
        };
        let path = entry.into_path();

        if path == key.root {
            continue;
        }

        if let Some(parent) = path.parent()
            && !directory_paths.contains(parent)
        {
            directory_paths.insert(parent.to_path_buf());
        }

        let metadata = match walker_metadata {
            Some(Ok(metadata)) => metadata,
            Some(Err(err)) => {
                let message = err.to_string();
                return Err(err
                    .into_io_error()
                    .map_or(ScopeCacheError::Walk(message), ScopeCacheError::Io));
            }
            None => fs::symlink_metadata(&path)?,
        };
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
        directories.iter().map(|entry| entry.path.as_path()),
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
        stamp_observation_floor,
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

/// How recorded-absent controls may be validated without probing the disk.
///
/// Skipping is sound only when the caller has, in the same validation pass,
/// re-verified that the owning directory's stamp still matches AND the
/// recorded stamp is race-free (`mtime + SCOPE_STAMP_RACY_SLACK` at or before
/// the observation floor of the pass that proved the control absent): under
/// those conditions a direct-child control cannot have appeared without
/// changing the directory stamp, and the race-free rule guarantees the
/// recording probe already saw anything created in the stamp's granule.
#[derive(Clone, Copy, Debug)]
pub enum AbsentControlSkip<'a> {
    /// Probe every candidate control on disk (previous behavior).
    ProbeAll,
    /// Every recorded scope directory's stamp was re-verified this pass and
    /// is race-free (the search index's certified file-set state).
    AllVerifiedDirs,
    /// Only these directories' stamps were re-verified as race-free.
    VerifiedDirs(&'a BTreeSet<PathBuf>),
}

impl AbsentControlSkip<'_> {
    fn directory_is_covered(&self, directory: &Path) -> bool {
        match self {
            Self::ProbeAll => false,
            Self::AllVerifiedDirs => true,
            Self::VerifiedDirs(dirs) => dirs.contains(directory),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IgnoreControlKind {
    /// `<dir>/.ignore` or `<dir>/.gitignore`: a direct child of a stamped
    /// scope directory.
    DirectChild,
    /// `<dir>/.git/info/exclude`: only its `.git` ancestor is a direct child
    /// of a stamped directory.
    GitExclude,
    /// The gitconfig `core.excludesFile`, outside the scope entirely.
    Global,
}

struct IgnoreControlCandidate {
    reason: &'static str,
    kind: IgnoreControlKind,
    /// Owning scope directory for umbrella decisions; `None` for the global
    /// control.
    directory: Option<PathBuf>,
}

pub fn ignore_fingerprint_change_reason<I, P>(
    root: &Path,
    directories: I,
    no_ignore: bool,
    expected: Option<&IgnoreFingerprint>,
    absent_skip: AbsentControlSkip<'_>,
    deadline: Instant,
) -> Result<Option<&'static str>, ScopeCacheError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    // A fingerprint exists exactly when ignore rules apply, so presence must
    // match between the recorded state and the requested flags.
    let Some(expected) = expected else {
        return Ok(if no_ignore {
            None
        } else {
            Some("ignore_rules_changed")
        });
    };
    if no_ignore {
        return Ok(Some("ignore_rules_changed"));
    }

    // Merge-join the freshly enumerated control set (sorted BTreeMap order)
    // against the recorded entries (built in the same order). Each control is
    // validated in place; content is only re-hashed when its stamp cannot
    // prove the file unchanged, instead of unconditionally re-reading every
    // existing ignore file on every query.
    let controls = enumerate_ignore_controls(root, directories, deadline)?;
    let mut expected_entries = expected.entries.iter().peekable();

    for (path, candidate) in controls {
        check_deadline(deadline)?;
        if let Some(entry) = expected_entries.peek()
            && entry.path < path
        {
            // A recorded control vanished from the candidate set.
            return Ok(Some(entry.reason));
        }
        let matching = match expected_entries.peek() {
            Some(entry) if entry.path == path => {
                expected_entries.next().expect("peeked entry present")
            }
            // Candidate absent from the recorded fingerprint: control set grew.
            _ => return Ok(Some(candidate.reason)),
        };
        if matching.stamp.is_none()
            && absent_control_still_absent(&candidate, expected, absent_skip)
        {
            continue;
        }
        if !ignore_control_stamp_is_fresh(&path, matching.stamp.as_ref(), deadline)? {
            return Ok(Some(matching.reason));
        }
    }

    if let Some(entry) = expected_entries.next() {
        return Ok(Some(entry.reason));
    }

    Ok(None)
}

/// Whether a control recorded as absent can be concluded still absent from
/// directory-stamp verification alone, without a disk probe.
fn absent_control_still_absent(
    candidate: &IgnoreControlCandidate,
    expected: &IgnoreFingerprint,
    absent_skip: AbsentControlSkip<'_>,
) -> bool {
    let Some(directory) = candidate.directory.as_deref() else {
        return false;
    };
    if !absent_skip.directory_is_covered(directory) {
        return false;
    }
    match candidate.kind {
        // A direct-child control appearing bumps the directory stamp itself.
        IgnoreControlKind::DirectChild => true,
        // The exclude file can only appear without touching a stamped
        // directory if a `.git` directory already existed at build time.
        IgnoreControlKind::GitExclude => !expected.git_dirs.contains(directory),
        IgnoreControlKind::Global => false,
    }
}

pub fn build_ignore_fingerprint<I, P>(
    root: &Path,
    directories: I,
    no_ignore: bool,
    deadline: Instant,
) -> Result<Option<IgnoreFingerprint>, ScopeCacheError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    if no_ignore {
        return Ok(None);
    }

    let directories: Vec<PathBuf> = directories
        .into_iter()
        .map(|directory| directory.as_ref().to_path_buf())
        .collect();
    let controls = enumerate_ignore_controls(root, directories.iter(), deadline)?;
    let mut entries = Vec::with_capacity(controls.len());
    for (path, candidate) in controls {
        check_deadline(deadline)?;
        entries.push(IgnoreFingerprintEntry {
            stamp: ignore_control_stamp(&path, deadline)?,
            path,
            reason: candidate.reason,
        });
    }

    let mut git_dirs = BTreeSet::new();
    for directory in &directories {
        check_deadline(deadline)?;
        if directory.join(".git").is_dir() {
            git_dirs.insert(directory.clone());
        }
    }

    Ok(Some(IgnoreFingerprint { entries, git_dirs }))
}

fn enumerate_ignore_controls<I, P>(
    root: &Path,
    directories: I,
    deadline: Instant,
) -> Result<BTreeMap<PathBuf, IgnoreControlCandidate>, ScopeCacheError>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut controls = BTreeMap::<PathBuf, IgnoreControlCandidate>::new();
    let push_directory_controls =
        |controls: &mut BTreeMap<PathBuf, IgnoreControlCandidate>, directory: &Path| {
            controls
                .entry(directory.join(".ignore"))
                .or_insert_with(|| IgnoreControlCandidate {
                    reason: "ignore_file_changed",
                    kind: IgnoreControlKind::DirectChild,
                    directory: Some(directory.to_path_buf()),
                });
            controls
                .entry(directory.join(".gitignore"))
                .or_insert_with(|| IgnoreControlCandidate {
                    reason: "gitignore_changed",
                    kind: IgnoreControlKind::DirectChild,
                    directory: Some(directory.to_path_buf()),
                });
            controls
                .entry(directory.join(".git").join("info").join("exclude"))
                .or_insert_with(|| IgnoreControlCandidate {
                    reason: "git_exclude_changed",
                    kind: IgnoreControlKind::GitExclude,
                    directory: Some(directory.to_path_buf()),
                });
        };
    for directory in directories {
        check_deadline(deadline)?;
        push_directory_controls(&mut controls, directory.as_ref());
    }
    if let Some(path) = ignore::gitignore::gitconfig_excludes_path() {
        controls
            .entry(path)
            .or_insert_with(|| IgnoreControlCandidate {
                reason: "global_ignore_changed",
                kind: IgnoreControlKind::Global,
                directory: None,
            });
    }
    if controls.is_empty() {
        push_directory_controls(&mut controls, root);
    }
    Ok(controls)
}

fn ignore_control_stamp(
    path: &Path,
    deadline: Instant,
) -> Result<Option<IgnoreControlStamp>, ScopeCacheError> {
    check_deadline(deadline)?;
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            // Windows cannot open directories for read without backup
            // semantics; a non-file control is recorded as absent, matching
            // the previous stat-first behavior.
            return match fs::metadata(path) {
                Ok(metadata) if !metadata.is_file() => Ok(None),
                _ => Err(ScopeCacheError::Io(err)),
            };
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(None);
    }
    // Stamp (including by-handle change info) before hashing content so a
    // write racing the hash leaves the stamp observably stale.
    let mut stamp = metadata_stamp_from_metadata(&metadata);
    attach_handle_change_info_to_stamp(&mut stamp, &file);
    let content_hash = content_hash_from_reader(&mut file, metadata.len(), deadline)?;
    check_deadline(deadline)?;
    Ok(Some(IgnoreControlStamp {
        metadata: stamp,
        content_hash,
    }))
}

/// Returns whether the on-disk state of `path` still matches the recorded
/// control stamp, re-hashing content only when no trusted change marker
/// (Unix ctime, Windows by-handle ChangeTime) can prove the file unchanged.
///
/// On Windows the stat fields and the ChangeTime re-observation both come
/// from one attribute-only handle: a path-based metadata query would open a
/// handle internally anyway, so splitting the two reads doubled the
/// `CreateFile` cost per control on every warm query.
fn ignore_control_stamp_is_fresh(
    path: &Path,
    expected: Option<&IgnoreControlStamp>,
    deadline: Instant,
) -> Result<bool, ScopeCacheError> {
    let (metadata, marker_proves_unchanged) = match observe_control_stamp(path, expected) {
        Ok(Some(observation)) => observation,
        Ok(None) => return Ok(expected.is_none()),
        Err(err) => return Err(ScopeCacheError::Io(err)),
    };
    let Some(expected) = expected else {
        return Ok(false);
    };

    // Any stat-visible difference is a change, exactly as the full stamp
    // comparison concluded before.
    if expected.metadata.len != metadata.len()
        || expected.metadata.modified != metadata.modified().ok()
        || !change_markers_match_stat_fields(expected.metadata.change_marker.as_ref(), &metadata)
    {
        return Ok(false);
    }

    if marker_proves_unchanged {
        return Ok(true);
    }

    // No trusted marker: fall back to the content hash comparison.
    check_deadline(deadline)?;
    let mut file = fs::File::open(path)?;
    let content_hash = content_hash_from_reader(&mut file, metadata.len(), deadline)?;
    Ok(content_hash == expected.content_hash)
}

#[cfg(not(windows))]
fn change_markers_match_stat_fields(
    expected: Option<&MetadataChangeMarker>,
    metadata: &fs::Metadata,
) -> bool {
    expected.copied() == metadata_change_marker(metadata)
}

#[cfg(windows)]
fn change_markers_match_stat_fields(
    expected: Option<&MetadataChangeMarker>,
    metadata: &fs::Metadata,
) -> bool {
    match (expected, metadata_change_marker(metadata)) {
        (Some(expected), Some(current)) => expected.stat_fields() == current.stat_fields(),
        (None, None) => true,
        _ => false,
    }
}

/// Whether the recorded marker alone proves content unchanged once the
/// stat-visible fields match. On Unix marker equality includes ctime, which
/// any rewrite bumps. (Windows proves this by re-observing the recorded
/// by-handle ChangeTime and file identity inside [`observe_control_stamp`].)
#[cfg(unix)]
fn change_marker_proves_unchanged(expected: &MetadataStamp, _path: &Path) -> bool {
    expected.change_marker.is_some()
}

#[cfg(not(any(unix, windows)))]
fn change_marker_proves_unchanged(_expected: &MetadataStamp, _path: &Path) -> bool {
    false
}

/// One-handle control observation: `Ok(None)` when the control is absent (or
/// not a regular file), otherwise the live metadata plus whether the recorded
/// change marker proves the content unchanged.
#[cfg(windows)]
fn observe_control_stamp(
    path: &Path,
    expected: Option<&IgnoreControlStamp>,
) -> io::Result<Option<(fs::Metadata, bool)>> {
    let file = match open_for_attributes(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            // Windows cannot open directories without backup semantics; a
            // non-file control is treated as absent, exactly as the
            // stat-first form concluded.
            return match fs::metadata(path) {
                Ok(metadata) if !metadata.is_file() => Ok(None),
                _ => Err(err),
            };
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let marker_proves_unchanged = expected
        .and_then(|expected| expected.metadata.change_marker.as_ref())
        .and_then(|marker| marker.handle_info)
        .is_some_and(|expected_handle| {
            windows_handle_change_info(&file).is_some_and(|current| current == expected_handle)
        });
    Ok(Some((metadata, marker_proves_unchanged)))
}

#[cfg(not(windows))]
fn observe_control_stamp(
    path: &Path,
    expected: Option<&IgnoreControlStamp>,
) -> io::Result<Option<(fs::Metadata, bool)>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    let marker_proves_unchanged =
        expected.is_some_and(|expected| change_marker_proves_unchanged(&expected.metadata, path));
    Ok(Some((metadata, marker_proves_unchanged)))
}

fn content_hash_from_reader<R: Read>(
    reader: &mut R,
    len: u64,
    deadline: Instant,
) -> Result<u64, ScopeCacheError> {
    let mut hasher = DefaultHasher::new();
    len.hash(&mut hasher);
    let mut buffer = [0_u8; IGNORE_HASH_BUFFER_BYTES];
    loop {
        check_deadline(deadline)?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.write(&buffer[..read]);
    }
    Ok(hasher.finish())
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
        handle_info: None,
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
            max_depth: None,
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
    fn repo_scope_snapshot_preserves_hidden_and_ignore_flags() {
        let dir = TestDir::new("repo-snapshot-hidden-ignore");
        write_file(&dir.path().join("visible.txt"), "visible");
        write_file(&dir.path().join(".hidden.txt"), "hidden");
        write_file(&dir.path().join("ignored.txt"), "ignored");
        write_file(&dir.path().join(".gitignore"), "ignored.txt\n");

        let cache = RepoScopeCache::new(RepoScopeCacheLimits {
            max_entries: 8,
            max_files_total: 1_000,
            full_validate_interval: 32,
        });

        let default_snapshot = cache
            .get_or_build(&repo_key(dir.path()), deadline())
            .expect("default snapshot");
        let default_paths = default_snapshot
            .entries
            .iter()
            .map(|entry| entry.rendered_path.as_str())
            .collect::<Vec<_>>();
        assert!(default_paths.contains(&"visible.txt"));
        assert!(!default_paths.contains(&".hidden.txt"));
        assert!(!default_paths.contains(&"ignored.txt"));

        let mut hidden_key = repo_key(dir.path());
        hidden_key.hidden = true;
        let hidden_snapshot = cache
            .get_or_build(&hidden_key, deadline())
            .expect("hidden snapshot");
        let hidden_paths = hidden_snapshot
            .entries
            .iter()
            .map(|entry| entry.rendered_path.as_str())
            .collect::<Vec<_>>();
        assert!(hidden_paths.contains(&".hidden.txt"));
        assert!(!hidden_paths.contains(&"ignored.txt"));

        let mut no_ignore_key = repo_key(dir.path());
        no_ignore_key.no_ignore = true;
        let no_ignore_snapshot = cache
            .get_or_build(&no_ignore_key, deadline())
            .expect("no-ignore snapshot");
        let no_ignore_paths = no_ignore_snapshot
            .entries
            .iter()
            .map(|entry| entry.rendered_path.as_str())
            .collect::<Vec<_>>();
        assert!(no_ignore_paths.contains(&"ignored.txt"));
        assert!(!no_ignore_paths.contains(&".hidden.txt"));
    }

    #[test]
    fn repo_scope_snapshot_respects_max_depth() {
        let dir = TestDir::new("repo-snapshot-max-depth");
        write_file(&dir.path().join("root.txt"), "root");
        fs::create_dir_all(dir.path().join("nested")).expect("create nested");
        write_file(&dir.path().join("nested").join("child.txt"), "child");

        let cache = RepoScopeCache::new(RepoScopeCacheLimits {
            max_entries: 8,
            max_files_total: 1_000,
            full_validate_interval: 32,
        });
        let key = RepoScopeKey {
            root: dir.path().to_path_buf(),
            hidden: false,
            follow: false,
            no_ignore: false,
            max_depth: Some(1),
        };

        let snapshot = cache
            .get_or_build(&key, deadline())
            .expect("bounded snapshot");
        assert!(
            snapshot
                .entries
                .iter()
                .any(|entry| entry.rendered_path == "root.txt"),
            "direct child file should be included: {:?}",
            snapshot.entries
        );
        assert!(
            snapshot
                .entries
                .iter()
                .any(|entry| entry.rendered_path == "nested"),
            "direct child directory should be included: {:?}",
            snapshot.entries
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| !entry.rendered_path.contains("child.txt")),
            "grandchild file must stay outside max_depth=1 snapshot: {:?}",
            snapshot.entries
        );
    }

    #[test]
    fn ignore_fingerprint_validation_reports_changes_and_passes_fresh_state() {
        let dir = TestDir::new("ignore-fingerprint-validation");
        write_file(&dir.path().join(".gitignore"), "aaa\n");
        let dirs = vec![dir.path().to_path_buf()];

        let fingerprint = build_ignore_fingerprint(dir.path(), &dirs, false, deadline())
            .expect("build fingerprint")
            .expect("ignore rules apply");

        assert_eq!(
            ignore_fingerprint_change_reason(
                dir.path(),
                &dirs,
                false,
                Some(&fingerprint),
                AbsentControlSkip::ProbeAll,
                deadline()
            )
            .expect("validate unchanged"),
            None,
            "unchanged controls must validate as fresh"
        );

        // Same-length in-place rewrite must be detected.
        write_file(&dir.path().join(".gitignore"), "bbb\n");
        assert_eq!(
            ignore_fingerprint_change_reason(
                dir.path(),
                &dirs,
                false,
                Some(&fingerprint),
                AbsentControlSkip::ProbeAll,
                deadline()
            )
            .expect("validate rewrite"),
            Some("gitignore_changed"),
        );

        // Restore, then create a control recorded as absent.
        write_file(&dir.path().join(".gitignore"), "aaa\n");
        let fingerprint = build_ignore_fingerprint(dir.path(), &dirs, false, deadline())
            .expect("rebuild fingerprint")
            .expect("ignore rules apply");
        write_file(&dir.path().join(".ignore"), "fresh\n");
        assert_eq!(
            ignore_fingerprint_change_reason(
                dir.path(),
                &dirs,
                false,
                Some(&fingerprint),
                AbsentControlSkip::ProbeAll,
                deadline()
            )
            .expect("validate created control"),
            Some("ignore_file_changed"),
        );
    }

    #[test]
    fn absent_control_probes_skip_only_under_verified_race_free_directories() {
        let dir = TestDir::new("ignore-umbrella-skip");
        write_file(&dir.path().join(".gitignore"), "aaa\n");
        let dirs = vec![dir.path().to_path_buf()];

        let fingerprint = build_ignore_fingerprint(dir.path(), &dirs, false, deadline())
            .expect("build fingerprint")
            .expect("ignore rules apply");

        // Create a control that the fingerprint recorded as absent. A caller
        // that has re-verified this directory's stamp as race-free may skip
        // the existence probe, so the change is (by contract) not visible
        // through this path — the directory stamp itself is the detector.
        write_file(&dir.path().join(".ignore"), "fresh\n");
        let eligible: BTreeSet<PathBuf> = dirs.iter().cloned().collect();
        assert_eq!(
            ignore_fingerprint_change_reason(
                dir.path(),
                &dirs,
                false,
                Some(&fingerprint),
                AbsentControlSkip::VerifiedDirs(&eligible),
                deadline()
            )
            .expect("validate with umbrella"),
            None,
            "recorded-absent controls under verified directories skip probes"
        );

        // Without eligibility the probe still runs and detects the file.
        let no_dirs = BTreeSet::new();
        assert_eq!(
            ignore_fingerprint_change_reason(
                dir.path(),
                &dirs,
                false,
                Some(&fingerprint),
                AbsentControlSkip::VerifiedDirs(&no_dirs),
                deadline()
            )
            .expect("validate without umbrella"),
            Some("ignore_file_changed"),
        );

        // Content changes to a PRESENT control are always detected, umbrella
        // or not.
        write_file(&dir.path().join(".gitignore"), "bbb\n");
        assert_eq!(
            ignore_fingerprint_change_reason(
                dir.path(),
                &dirs,
                false,
                Some(&fingerprint),
                AbsentControlSkip::AllVerifiedDirs,
                deadline()
            )
            .expect("validate present-control change"),
            Some("gitignore_changed"),
        );
    }

    #[test]
    fn git_exclude_probe_survives_umbrella_when_git_dir_was_recorded() {
        let dir = TestDir::new("ignore-umbrella-git-exclude");
        let git_info = dir.path().join(".git").join("info");
        fs::create_dir_all(&git_info).expect("create .git/info");
        let dirs = vec![dir.path().to_path_buf()];

        let fingerprint = build_ignore_fingerprint(dir.path(), &dirs, false, deadline())
            .expect("build fingerprint")
            .expect("ignore rules apply");

        // The exclude file appears inside a pre-existing `.git` directory:
        // no stamped scope directory changes, so the umbrella must NOT skip
        // this probe.
        write_file(&git_info.join("exclude"), "secret\n");
        assert_eq!(
            ignore_fingerprint_change_reason(
                dir.path(),
                &dirs,
                false,
                Some(&fingerprint),
                AbsentControlSkip::AllVerifiedDirs,
                deadline()
            )
            .expect("validate exclude creation"),
            Some("git_exclude_changed"),
            "exclude probes must keep running when .git existed at build time"
        );
    }

    #[test]
    fn ignore_fingerprint_validation_detects_control_set_growth() {
        let dir = TestDir::new("ignore-fingerprint-set-growth");
        write_file(&dir.path().join(".gitignore"), "aaa\n");
        let subdir = dir.path().join("nested");
        fs::create_dir_all(&subdir).expect("create nested dir");

        let built_dirs = vec![dir.path().to_path_buf()];
        let fingerprint = build_ignore_fingerprint(dir.path(), &built_dirs, false, deadline())
            .expect("build fingerprint")
            .expect("ignore rules apply");

        // Validating with an extra directory yields candidate controls the
        // recorded fingerprint has never seen.
        let grown_dirs = vec![dir.path().to_path_buf(), subdir];
        let reason = ignore_fingerprint_change_reason(
            dir.path(),
            &grown_dirs,
            false,
            Some(&fingerprint),
            AbsentControlSkip::ProbeAll,
            deadline(),
        )
        .expect("validate grown set");
        assert!(
            reason.is_some(),
            "a grown control set must be reported as changed"
        );
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
