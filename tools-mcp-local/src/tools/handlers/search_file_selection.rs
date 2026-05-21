//! Shared file selection for the `Search` handler backends.

use super::search_contract::NormalizedSearchRequest;
use crate::tools::scope_cache::{
    RecursiveScopeSnapshot, RepoScopeKey, ScopeCacheError, ScopeFileType, repo_scope_cache,
};
use glob::{MatchOptions, Pattern};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tools_mcp_core::cancellation::current_cancellation_token;

#[derive(Clone, Debug)]
pub(super) struct FileSelectionError {
    pub(super) error_type: &'static str,
    pub(super) fallback_reason: &'static str,
    pub(super) fallback_allowed: bool,
    pub(super) message: String,
    pub(super) timed_out: bool,
}

impl FileSelectionError {
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
            message: "search file selection timed out".to_string(),
            timed_out: true,
        }
    }

    fn cancelled() -> Self {
        Self {
            error_type: "cancelled",
            fallback_reason: "cancelled",
            fallback_allowed: false,
            message: "search file selection cancelled".to_string(),
            timed_out: false,
        }
    }

    #[allow(dead_code)]
    fn walk(err: ignore::Error) -> Self {
        Self::new(
            "search_index_incomplete",
            "walk_error",
            format!("memory search walk failed: {err}"),
        )
    }
}

#[derive(Clone, Debug)]
pub(super) struct FileSelector {
    root_arg: String,
    root: PathBuf,
    include_hidden: bool,
    follow_links: bool,
    no_ignore: bool,
    glob_key: Vec<String>,
    glob_filter: Option<SearchGlobFilter>,
}

#[derive(Clone, Debug)]
pub(super) struct MemoryScopeDiscovery {
    pub(super) files: Vec<PathBuf>,
    pub(super) directories: Vec<PathBuf>,
}

impl FileSelector {
    pub(super) fn for_memory(req: &NormalizedSearchRequest) -> Result<Self, FileSelectionError> {
        let glob_filter = SearchGlobFilter::for_memory(req, req.hidden())?;
        Ok(Self {
            root_arg: req.root().to_string(),
            root: PathBuf::from(req.root()),
            include_hidden: req.hidden(),
            follow_links: false,
            no_ignore: req.no_ignore(),
            glob_key: glob_filter
                .as_ref()
                .map_or_else(Vec::new, |filter| filter.cache_key.clone()),
            glob_filter,
        })
    }

    pub(super) fn for_ugrep_path_list(req: &NormalizedSearchRequest) -> Option<Self> {
        let glob_filter = SearchGlobFilter::for_ugrep_path_list(req, req.hidden())?;
        Some(Self {
            root_arg: req.root().to_string(),
            root: PathBuf::from(req.root()),
            include_hidden: req.hidden(),
            follow_links: req.follow(),
            no_ignore: req.no_ignore(),
            glob_key: glob_filter.cache_key.clone(),
            glob_filter: Some(glob_filter),
        })
    }

    pub(super) fn root_arg(&self) -> &str {
        &self.root_arg
    }

    pub(super) fn include_hidden(&self) -> bool {
        self.include_hidden
    }

    pub(super) fn follow_links(&self) -> bool {
        self.follow_links
    }

    pub(super) fn no_ignore(&self) -> bool {
        self.no_ignore
    }

    pub(super) fn glob_key(&self) -> &[String] {
        &self.glob_key
    }

    pub(super) fn discover_memory_files(
        &self,
        deadline: Option<Instant>,
    ) -> Result<Vec<PathBuf>, FileSelectionError> {
        self.discover_memory_scope(deadline)
            .map(|scope| scope.files)
    }

    pub(super) fn discover_memory_scope(
        &self,
        deadline: Option<Instant>,
    ) -> Result<MemoryScopeDiscovery, FileSelectionError> {
        let cancel_token = current_cancellation_token();
        let cancel = cancel_token.as_ref();

        // When the search root is a single file (not a directory), the shared
        // repo scope cache cannot describe it (the cache treats `root` as the
        // anchor of a recursive walk and skips emitting it as a file entry).
        // Fall back to the original walker semantics for that narrow case so
        // single-file `path` queries continue to surface their target file.
        if self.root.is_file() {
            return self.discover_memory_scope_via_walker(deadline);
        }

        // Reuse the shared repo scope cache so cross-call repeated scans share a
        // single walker invocation. The cache builder mirrors the WalkBuilder
        // configuration below (see tools-mcp-local/src/tools/scope_cache.rs).
        let snapshot = self.scope_snapshot(deadline)?;

        let mut files = Vec::new();
        let mut directories = Vec::with_capacity(snapshot.directories.len());

        // Mirror the existing walker: the root directory is always part of the
        // observed directory set, plus every directory the walker traversed.
        for fingerprint in &snapshot.directories {
            directories.push(fingerprint.path.clone());
        }

        for entry in &snapshot.entries {
            if let Some(token) = cancel
                && token.is_cancelled()
            {
                return Err(FileSelectionError::cancelled());
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                return Err(FileSelectionError::timeout());
            }

            match entry.file_type {
                ScopeFileType::Dir => {
                    // Directories already collected via snapshot.directories.
                    continue;
                }
                ScopeFileType::Symlink if !self.follow_links => {
                    // Match existing walker: skip symlinks when follow_links is false.
                    continue;
                }
                ScopeFileType::File | ScopeFileType::Symlink => {}
            }

            let path = &entry.path;
            if self.matches_glob(path) {
                if path_has_line_separator(path) {
                    return Err(FileSelectionError::new(
                        "search_index_incomplete",
                        "unsafe_path_separator",
                        format!(
                            "memory search cannot safely render a path containing LF/CR bytes: {:?}",
                            path
                        ),
                    ));
                }
                files.push(path.clone());
            }
        }

        files.sort();
        directories.sort();
        directories.dedup();
        Ok(MemoryScopeDiscovery { files, directories })
    }

    /// Fallback walker used when the search root is a file (the shared scope
    /// cache is not designed for single-file anchors). Mirrors the previous
    /// pre-cache semantics: traverse with `WalkBuilder`, push directories into
    /// `directories`, push files into `files`, and skip symlinks when
    /// `follow_links` is false.
    fn discover_memory_scope_via_walker(
        &self,
        deadline: Option<Instant>,
    ) -> Result<MemoryScopeDiscovery, FileSelectionError> {
        let cancel_token = current_cancellation_token();
        let cancel = cancel_token.as_ref();

        let mut files = Vec::new();
        let mut directories = Vec::new();

        for entry in self.walk_builder().build() {
            if let Some(token) = cancel
                && token.is_cancelled()
            {
                return Err(FileSelectionError::cancelled());
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                return Err(FileSelectionError::timeout());
            }

            let entry = entry.map_err(FileSelectionError::walk)?;
            let file_type = entry.file_type();

            if file_type.is_some_and(|ft| ft.is_symlink()) && !self.follow_links {
                continue;
            }

            let path = entry.into_path();
            if file_type.is_some_and(|ft| ft.is_dir()) {
                directories.push(path);
                continue;
            }

            if self.matches_glob(&path) {
                if path_has_line_separator(&path) {
                    return Err(FileSelectionError::new(
                        "search_index_incomplete",
                        "unsafe_path_separator",
                        format!(
                            "memory search cannot safely render a path containing LF/CR bytes: {:?}",
                            path
                        ),
                    ));
                }
                files.push(path);
            }
        }

        files.sort();
        directories.sort();
        directories.dedup();
        Ok(MemoryScopeDiscovery { files, directories })
    }

    /// Retrieve the cached `RecursiveScopeSnapshot` for this selector, building it on
    /// miss. Falls back to a 30s deadline when the caller did not pass one so the
    /// shared cache never becomes the bottleneck for callers without an explicit
    /// timeout (typical search timeouts are 5-15s).
    fn scope_snapshot(
        &self,
        deadline: Option<Instant>,
    ) -> Result<Arc<RecursiveScopeSnapshot>, FileSelectionError> {
        let key = RepoScopeKey {
            root: self.root.clone(),
            hidden: self.include_hidden,
            follow: self.follow_links,
            no_ignore: self.no_ignore,
        };
        let effective_deadline =
            deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
        repo_scope_cache()
            .get_or_build(&key, effective_deadline)
            .map_err(|err| match err {
                ScopeCacheError::Timeout => FileSelectionError::timeout(),
                ScopeCacheError::Walk(message) => FileSelectionError::new(
                    "search_index_incomplete",
                    "walk_error",
                    format!("memory search walk failed: {message}"),
                ),
                ScopeCacheError::Io(io_err) => FileSelectionError::new(
                    "search_index_incomplete",
                    "walk_error",
                    format!("memory search walk failed: {io_err}"),
                ),
            })
    }

    pub(super) fn resolve_ugrep_path_list(
        &self,
        deadline: Option<Instant>,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let cancel_token = current_cancellation_token();
        let cancel = cancel_token.as_ref();

        let mut files = Vec::new();
        for entry in self.walk_builder().build() {
            if let Some(token) = cancel
                && token.is_cancelled()
            {
                anyhow::bail!("search file selection cancelled while resolving ugrep path list");
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                anyhow::bail!("search file selection timed out while resolving ugrep path list");
            }

            let entry = entry?;
            if self.should_skip_entry(&entry) {
                continue;
            }

            let path = entry.into_path();
            if self.matches_glob(&path) {
                if path_has_line_separator(&path) {
                    anyhow::bail!(
                        "search aborted: matched path contains LF/CR bytes that cannot \
                         be safely passed to ugrep --from=- (offending path: {:?})",
                        path
                    );
                }
                if path_requires_lossy_utf8(&path) {
                    anyhow::bail!(
                        "search aborted: matched path contains non-UTF-8 bytes that cannot \
                         be safely reconciled with ugrep text output (offending path: {:?})",
                        path
                    );
                }
                files.push(path);
            }
        }

        files.sort();
        Ok(files)
    }

    pub(super) fn render_path(&self, path: &Path) -> String {
        if is_current_dir_root_arg(&self.root_arg)
            && let Ok(stripped) = path.strip_prefix(".")
        {
            return stripped.display().to_string();
        }
        path.display().to_string()
    }

    fn walk_builder(&self) -> WalkBuilder {
        let mut builder = WalkBuilder::new(&self.root);
        builder
            .hidden(!self.include_hidden)
            .follow_links(self.follow_links)
            .ignore(!self.no_ignore)
            .git_ignore(!self.no_ignore)
            .git_global(!self.no_ignore)
            .git_exclude(!self.no_ignore);
        builder
    }

    fn should_skip_entry(&self, entry: &ignore::DirEntry) -> bool {
        entry.file_type().is_some_and(|ft| ft.is_dir())
            || (entry.file_type().is_some_and(|ft| ft.is_symlink()) && !self.follow_links)
    }

    fn matches_glob(&self, path: &Path) -> bool {
        self.glob_filter
            .as_ref()
            .is_none_or(|filter| filter.is_match(&self.root, path))
    }
}

pub(super) fn resolve_ugrep_path_list(
    req: &NormalizedSearchRequest,
    deadline: Option<Instant>,
) -> anyhow::Result<Option<Vec<PathBuf>>> {
    let Some(selector) = FileSelector::for_ugrep_path_list(req) else {
        return Ok(None);
    };
    selector.resolve_ugrep_path_list(deadline).map(Some)
}

#[derive(Clone, Debug)]
struct CompiledGlob {
    pattern: Pattern,
    match_basename: bool,
    pattern_key: String,
}

#[derive(Clone, Debug)]
struct SearchGlobFilter {
    patterns: Vec<CompiledGlob>,
    match_options: MatchOptions,
    cache_key: Vec<String>,
}

impl SearchGlobFilter {
    fn for_memory(
        req: &NormalizedSearchRequest,
        include_hidden: bool,
    ) -> Result<Option<Self>, FileSelectionError> {
        compile_globs(req, include_hidden, GlobCompileMode::Memory)
    }

    fn for_ugrep_path_list(req: &NormalizedSearchRequest, include_hidden: bool) -> Option<Self> {
        compile_globs(req, include_hidden, GlobCompileMode::UgrepPathList).ok()?
    }

    fn is_match(&self, root: &Path, path: &Path) -> bool {
        self.patterns
            .iter()
            .any(|compiled| self.compiled_pattern_matches(compiled, root, path))
    }

    fn compiled_pattern_matches(&self, compiled: &CompiledGlob, root: &Path, path: &Path) -> bool {
        if compiled.match_basename {
            return path.file_name().is_some_and(|file_name| {
                compiled
                    .pattern
                    .matches_path_with(Path::new(file_name), self.match_options)
            });
        }

        if let Some(rendered_path) = rendered_ugrep_path(root, path)
            && compiled
                .pattern
                .matches_path_with(Path::new(&rendered_path), self.match_options)
        {
            return true;
        }

        compiled.pattern.matches_path_with(path, self.match_options)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobCompileMode {
    Memory,
    UgrepPathList,
}

fn compile_globs(
    req: &NormalizedSearchRequest,
    include_hidden: bool,
    mode: GlobCompileMode,
) -> Result<Option<SearchGlobFilter>, FileSelectionError> {
    let globs = req.normalized_globs();
    if globs.is_empty() {
        return Ok(None);
    }

    let mut patterns = Vec::new();
    for raw_glob in globs {
        let trimmed = raw_glob.trim();
        if trimmed.is_empty() {
            continue;
        }

        if contains_unsupported_glob_syntax(trimmed) {
            return match mode {
                GlobCompileMode::Memory => Err(FileSelectionError::new(
                    "unsupported_search_option",
                    "unsupported_glob_syntax",
                    format!(
                        "memory search cannot preserve Search glob semantics for pattern: {trimmed}"
                    ),
                )),
                GlobCompileMode::UgrepPathList => Ok(None),
            };
        }

        let match_basename = !contains_ugrep_path_separator(trimmed);
        let normalized = normalize_ugrep_glob_pattern(trimmed);
        let pattern = match Pattern::new(&normalized) {
            Ok(pattern) => pattern,
            Err(err) => {
                return match mode {
                    GlobCompileMode::Memory => Err(FileSelectionError::new(
                        "unsupported_search_option",
                        "invalid_glob",
                        format!("memory search received invalid glob pattern {trimmed:?}: {err}"),
                    )),
                    GlobCompileMode::UgrepPathList => Ok(None),
                };
            }
        };

        let pattern_key = glob_pattern_key(match_basename, &normalized);
        patterns.push(CompiledGlob {
            pattern,
            match_basename,
            pattern_key,
        });
    }

    if patterns.is_empty() {
        return Ok(None);
    }

    let mut cache_key: Vec<String> = patterns
        .iter()
        .map(|compiled| compiled.pattern_key.clone())
        .collect();
    cache_key.sort();
    cache_key.dedup();

    Ok(Some(SearchGlobFilter {
        patterns,
        cache_key,
        match_options: MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: !include_hidden,
        },
    }))
}

fn path_relative_to_root<'a>(root: &Path, path: &'a Path) -> Option<&'a Path> {
    if let Ok(relative) = path.strip_prefix(root) {
        if !relative.as_os_str().is_empty() {
            return Some(relative);
        }
    }

    if root.is_file() {
        return path.file_name().map(Path::new);
    }

    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative);
    }

    if root.to_str().is_some_and(is_current_dir_root_arg) {
        return Some(path);
    }

    None
}

fn rendered_ugrep_path(root: &Path, path: &Path) -> Option<String> {
    // Slash globs match against the search-root-relative form of each path,
    // regardless of whether the search root is `.`, a relative subdir, or an
    // absolute path. This preserves the contract guarded by
    // `tools-mcp-server/tests/integration_test.rs::
    // test_search_ugrep_fallback_preserves_slash_glob_or_semantics`, where
    // a glob like `tools-mcp-server/tests/integration_test.rs` must match
    // `<search_root>/tools-mcp-server/tests/integration_test.rs`.
    path_relative_to_root(root, path)
        .map(|relative| relative.display().to_string())
        .or_else(|| Some(path.display().to_string()))
}

fn is_current_dir_root_arg(root: &str) -> bool {
    matches!(root, "." | "./") || (cfg!(windows) && root == ".\\")
}

fn contains_ugrep_path_separator(pattern: &str) -> bool {
    pattern.contains('/')
}

fn contains_unsupported_glob_syntax(pattern: &str) -> bool {
    pattern.starts_with('!')
        || pattern.starts_with('^')
        || pattern.ends_with('/')
        || pattern.contains(',')
        || pattern.contains('\\')
        || pattern.contains('{')
        || pattern.contains('}')
}

fn normalize_ugrep_glob_pattern(pattern: &str) -> String {
    let mut normalized = pattern;
    loop {
        if let Some(stripped) = normalized.strip_prefix("./") {
            normalized = stripped;
        } else if let Some(stripped) = normalized.strip_prefix('/') {
            if cfg!(windows) && normalized.starts_with("//") {
                break;
            }
            normalized = stripped;
        } else {
            break;
        }
    }
    normalized.to_string()
}

fn glob_pattern_key(match_basename: bool, pattern: &str) -> String {
    let mode = if match_basename { "basename" } else { "path" };
    format!("{mode}:{pattern}")
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
pub(super) fn path_has_line_separator(path: &Path) -> bool {
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
pub(super) fn path_has_line_separator(path: &Path) -> bool {
    path.to_string_lossy()
        .bytes()
        .any(|b| matches!(b, b'\n' | b'\r'))
}

#[cfg(unix)]
fn path_requires_lossy_utf8(path: &Path) -> bool {
    path.as_os_str().to_str().is_none()
}

#[cfg(not(unix))]
fn path_requires_lossy_utf8(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::super::search_contract::SearchRequest;
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn workspace_test_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::current_dir()
            .expect("current dir")
            .join("target")
            .join("test-work")
            .join(format!(
                "search-file-selection-{name}-{}-{unique}",
                std::process::id()
            ))
    }

    fn search_request(root: String, glob: &str) -> NormalizedSearchRequest {
        SearchRequest {
            pattern: "needle".to_string(),
            path: Some(root),
            case: Some("sensitive".to_string()),
            fixed_strings: Some(true),
            word_regexp: Some(false),
            glob: Some(vec![glob.to_string()]),
            hidden: Some(true),
            follow: Some(false),
            no_ignore: Some(true),
            context: None,
            max_results: None,
            timeout_ms: None,
            fuzzy: None,
        }
        .normalize()
    }

    fn write_fixture(root: &Path) {
        fs::create_dir_all(root.join("src").join("nested")).expect("create fixture dirs");
        fs::write(root.join("root.rs"), "needle\n").expect("write root");
        fs::write(root.join("src").join("lib.rs"), "needle\n").expect("write lib");
        fs::write(root.join("src").join("root.rs"), "needle\n").expect("write nested root");
        fs::write(root.join("src").join("nested").join("deep.rs"), "needle\n").expect("write deep");
        fs::write(root.join("notes.md"), "needle\n").expect("write notes");
    }

    fn discover(root: String, glob: &str) -> Result<Vec<PathBuf>, FileSelectionError> {
        let req = search_request(root, glob);
        FileSelector::for_memory(&req)?.discover_memory_files(None)
    }

    #[test]
    fn basename_glob_matches_files_at_any_depth() {
        let root = workspace_test_dir("basename_glob_matches_files_at_any_depth");
        let _ = fs::remove_dir_all(&root);
        write_fixture(&root);

        let files = discover(root.display().to_string(), "*.rs").expect("discover files");

        assert!(files.contains(&root.join("root.rs")));
        assert!(files.contains(&root.join("src").join("lib.rs")));
        assert!(files.contains(&root.join("src").join("nested").join("deep.rs")));
        assert!(!files.contains(&root.join("notes.md")));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn slash_glob_is_root_relative_for_absolute_root() {
        let root = workspace_test_dir("slash_glob_is_root_relative_for_absolute_root");
        let _ = fs::remove_dir_all(&root);
        write_fixture(&root);
        let root_arg = root.display().to_string();

        let direct = discover(root_arg.clone(), "src/*.rs").expect("discover files");
        let globstar = discover(root_arg, "**/src/*.rs").expect("discover globstar files");

        let expected = vec![
            root.join("src").join("lib.rs"),
            root.join("src").join("root.rs"),
        ];
        assert_eq!(
            direct, expected,
            "slash globs must match the root-relative form regardless of search-root shape"
        );
        assert_eq!(
            globstar, expected,
            "`**/` prefix must still resolve to the same root-relative files"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn slash_glob_is_root_relative_for_relative_root() {
        let root = workspace_test_dir("slash_glob_is_root_relative_for_relative_root");
        let _ = fs::remove_dir_all(&root);
        write_fixture(&root);
        let cwd = std::env::current_dir().expect("current dir");
        let relative_root = root
            .strip_prefix(&cwd)
            .expect("fixture under cwd")
            .display()
            .to_string();

        let direct = discover(relative_root.clone(), "src/*.rs").expect("discover files");
        let expected_root = PathBuf::from(&relative_root);
        let expected = vec![
            expected_root.join("src").join("lib.rs"),
            expected_root.join("src").join("root.rs"),
        ];
        assert_eq!(
            direct, expected,
            "slash globs are root-relative even when the search root itself is a relative path"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(windows)]
    fn windows_current_dir_backslash_root_renders_relative_paths() {
        let selector = FileSelector {
            root_arg: ".\\".to_string(),
            root: PathBuf::from("."),
            include_hidden: true,
            follow_links: false,
            no_ignore: true,
            glob_key: Vec::new(),
            glob_filter: None,
        };

        assert_eq!(
            selector.render_path(Path::new(".\\src\\lib.rs")),
            "src\\lib.rs"
        );
    }

    #[test]
    #[cfg(windows)]
    fn windows_drive_and_unc_roots_strip_prefix_for_relative_paths() {
        assert_eq!(
            path_relative_to_root(Path::new("C:\\"), Path::new("C:\\repo\\src\\lib.rs")),
            Some(Path::new("repo\\src\\lib.rs"))
        );
        assert_eq!(
            path_relative_to_root(
                Path::new("\\\\server\\share\\repo"),
                Path::new("\\\\server\\share\\repo\\src\\lib.rs")
            ),
            Some(Path::new("src\\lib.rs"))
        );
    }

    // `windows_absolute_drive_glob_matches_full_rendered_path` removed: it asserted the
    // now-rejected design where slash globs were matched against the full absolute
    // path. Under restored root-relative semantics an absolute-path glob like
    // `C:/<root>/src/*.rs` no longer matches files whose root-relative form is
    // `src/*.rs`. Coverage for the corrected behavior on Windows is provided by
    // `slash_glob_is_root_relative_for_absolute_root`, which runs on every
    // platform (search roots there are always absolute).

    #[test]
    #[cfg(windows)]
    fn windows_unc_glob_prefix_is_preserved() {
        assert_eq!(
            normalize_ugrep_glob_pattern("//server/share/src/*.rs"),
            "//server/share/src/*.rs"
        );
        assert_eq!(normalize_ugrep_glob_pattern("/src/*.rs"), "src/*.rs");
        assert_eq!(normalize_ugrep_glob_pattern("./src/*.rs"), "src/*.rs");
    }

    #[test]
    #[cfg(windows)]
    fn windows_path_separator_guard_checks_lossy_path_text() {
        assert!(path_has_line_separator(Path::new("unsafe\r\nname.txt")));
        assert!(!path_has_line_separator(Path::new("safe-name.txt")));
    }

    #[test]
    #[cfg(unix)]
    fn unix_memory_discovery_rejects_lf_cr_paths_before_rendering() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let root = workspace_test_dir("unix_memory_discovery_rejects_lf_cr_paths");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        let mut filename = OsString::from("unsafe");
        filename.push(OsString::from_vec(vec![b'\n']));
        filename.push("name.txt");
        fs::write(root.join(&filename), "needle\n").expect("write unsafe filename");

        let err = discover(root.display().to_string(), "*")
            .expect_err("memory discovery must reject LF/CR-bearing paths");

        assert_eq!(err.error_type, "search_index_incomplete");
        assert_eq!(err.fallback_reason, "unsafe_path_separator");
        assert!(err.fallback_allowed);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn unix_ugrep_path_list_rejects_non_utf8_matched_paths() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let root = workspace_test_dir("unix_ugrep_path_list_rejects_non_utf8_matched_paths");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        let filename = OsString::from_vec(b"nonutf8-\xff.txt".to_vec());
        fs::write(root.join(filename), "needle\n").expect("write non-UTF-8 filename");

        let req = search_request(root.display().to_string(), "*");
        let err = resolve_ugrep_path_list(&req, None).expect_err("non-UTF-8 path must abort");

        assert!(
            err.to_string().contains("non-UTF-8"),
            "expected non-UTF-8 rejection diagnostic, got: {err}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(unix)]
    fn unix_memory_discovery_skips_symlinks_without_following() {
        use std::os::unix::fs::symlink;

        let root = workspace_test_dir("unix_memory_discovery_skips_symlinks_without_following");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        let target = root.join("target.txt");
        let link = root.join("link.txt");
        fs::write(&target, "needle\n").expect("write target");
        symlink(&target, &link).expect("create symlink");

        let files = discover(root.display().to_string(), "*.txt").expect("discover files");

        assert!(files.contains(&target));
        assert!(
            !files.contains(&link),
            "memory discovery must not follow or index symlink entries"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ugrep_path_list_resolution_honors_deadline() {
        let root = workspace_test_dir("ugrep_path_list_resolution_honors_deadline");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("sample.txt"), "needle\n").expect("write fixture");

        let req = search_request(root.display().to_string(), "*.txt");
        let err = resolve_ugrep_path_list(&req, Some(Instant::now() - Duration::from_millis(1)))
            .expect_err("expired deadline must abort path-list resolution");

        assert!(
            err.to_string().contains("timed out"),
            "expected timeout diagnostic, got: {err}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unsupported_memory_globs_request_fallback() {
        let root = workspace_test_dir("unsupported_memory_globs_request_fallback");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");

        for glob in [
            "^*.md",
            "!*.md",
            "*.txt,*.md",
            "src\\*.rs",
            "src/",
            "*.{txt,md}",
        ] {
            let req = search_request(root.display().to_string(), glob);
            let err = FileSelector::for_memory(&req).expect_err("glob should fall back");
            assert_eq!(err.error_type, "unsupported_search_option", "glob: {glob}");
            assert_eq!(
                err.fallback_reason, "unsupported_glob_syntax",
                "glob: {glob}"
            );
            assert!(err.fallback_allowed, "glob: {glob}");
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unsupported_ugrep_path_list_globs_delegate_to_raw_ugrep() {
        let root = workspace_test_dir("unsupported_ugrep_path_list_globs_delegate_to_raw_ugrep");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");

        for glob in [
            "^*.md",
            "!*.md",
            "*.txt,*.md",
            "src\\*.rs",
            "src/",
            "*.{txt,md}",
        ] {
            let req = search_request(root.display().to_string(), glob);
            let resolved = resolve_ugrep_path_list(&req, None).expect("path-list resolution");
            assert_eq!(resolved, None, "glob: {glob}");
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scope_cache_returns_same_arc_for_repeated_key() {
        let root = workspace_test_dir("scope_cache_returns_same_arc_for_repeated_key");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("a.rs"), "needle\n").expect("write fixture a");
        fs::write(root.join("b.rs"), "needle\n").expect("write fixture b");

        let key = RepoScopeKey {
            root: fs::canonicalize(&root).expect("canonical root"),
            hidden: true,
            follow: false,
            no_ignore: true,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let cache = repo_scope_cache();
        let first = cache.get_or_build(&key, deadline).expect("first snapshot");
        let second = cache.get_or_build(&key, deadline).expect("second snapshot");

        assert!(
            Arc::ptr_eq(&first, &second),
            "scope cache must return the same Arc for an unchanged key"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
