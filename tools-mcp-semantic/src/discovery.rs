use anyhow::{Context, Result, anyhow, bail};
use ignore::WalkBuilder;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tools_mcp_core::cancellation::current_cancellation_token;

const INDEX_DIR_NAME: &str = ".tools-mcp";
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const INITIAL_FILE_CAPACITY: usize = 1024;

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceScope {
    pub(crate) workspace: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) index_dir: PathBuf,
    pub(crate) target_filter: PathFilter,
}

#[derive(Clone, Debug)]
pub(crate) enum PathFilter {
    Workspace,
    File(String),
    Directory(String),
}

impl PathFilter {
    pub(crate) fn contains(&self, relative_path: &str) -> bool {
        match self {
            Self::Workspace => true,
            Self::File(path) => relative_path == path,
            Self::Directory(path) => {
                relative_path == path
                    || relative_path
                        .strip_prefix(path)
                        .is_some_and(|rest| rest.starts_with('/'))
            }
        }
    }

    pub(crate) fn to_sql(&self) -> Option<String> {
        match self {
            Self::Workspace => None,
            Self::File(path) => Some(format!("path = '{}'", escape_sql_literal(path))),
            Self::Directory(path) => {
                let child_lower_bound = format!("{path}/");
                let child_upper_bound = format!("{path}0");
                Some(format!(
                    "(path = '{}' OR (path >= '{}' AND path < '{}'))",
                    escape_sql_literal(path),
                    escape_sql_literal(&child_lower_bound),
                    escape_sql_literal(&child_upper_bound)
                ))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FileCandidate {
    pub(crate) absolute_path: PathBuf,
    pub(crate) relative_path: String,
    pub(crate) language: String,
}

#[derive(Clone, Debug)]
pub(crate) struct DiscoveryOptions {
    pub(crate) path: String,
    pub(crate) hidden: bool,
    pub(crate) no_ignore: bool,
    pub(crate) limit: usize,
    pub(crate) timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct DiscoveryResult {
    pub(crate) scope: WorkspaceScope,
    pub(crate) files: Vec<FileCandidate>,
    pub(crate) skipped_files: usize,
    pub(crate) truncated: bool,
    pub(crate) timed_out: bool,
}

pub(crate) fn resolve_scope(path: &str) -> Result<WorkspaceScope> {
    let workspace = std::env::current_dir()
        .context("failed to read server working directory")?
        .canonicalize()
        .context("failed to resolve server working directory")?;
    let target = resolve_existing_under_workspace(Path::new(path), &workspace)?;
    let index_dir = workspace.join(INDEX_DIR_NAME).join("semantic-index");
    let target_filter = if target == workspace {
        PathFilter::Workspace
    } else {
        let relative = storage_relative_path(&workspace, &target)?;
        if target.is_file() {
            PathFilter::File(relative)
        } else {
            PathFilter::Directory(relative)
        }
    };

    Ok(WorkspaceScope {
        workspace,
        target,
        index_dir,
        target_filter,
    })
}

pub(crate) fn discover_files(options: DiscoveryOptions) -> Result<DiscoveryResult> {
    let scope = resolve_scope(&options.path)?;
    let deadline = Instant::now() + Duration::from_millis(options.timeout_ms);
    let cancel_token = current_cancellation_token();
    let mut files = Vec::with_capacity(options.limit.min(INITIAL_FILE_CAPACITY));
    let mut skipped_files = 0usize;
    let mut truncated = false;
    let mut timed_out = false;

    if scope.target.is_file() {
        if let Some(file) = file_candidate(&scope.workspace, &scope.target)? {
            files.push(file);
        } else {
            skipped_files = skipped_files.saturating_add(1);
        }
        return Ok(DiscoveryResult {
            scope,
            files,
            skipped_files,
            truncated,
            timed_out,
        });
    }

    if should_skip_path(&scope.target) {
        return Ok(DiscoveryResult {
            scope,
            files,
            skipped_files: skipped_files.saturating_add(1),
            truncated,
            timed_out,
        });
    }

    let pruned_entries = Arc::new(AtomicUsize::new(0));
    let pruned_entries_for_filter = Arc::clone(&pruned_entries);
    let mut builder = WalkBuilder::new(&scope.target);
    builder
        .hidden(!options.hidden)
        .ignore(!options.no_ignore)
        .git_ignore(!options.no_ignore)
        .git_exclude(!options.no_ignore)
        .git_global(!options.no_ignore)
        .follow_links(false)
        .filter_entry(move |entry| {
            let should_keep = !is_excluded_name(entry.file_name());
            if !should_keep {
                pruned_entries_for_filter.fetch_add(1, Ordering::Relaxed);
            }
            should_keep
        });

    for entry in builder.build() {
        if Instant::now() >= deadline {
            timed_out = true;
            truncated = true;
            break;
        }
        if cancel_token
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            bail!("semantic indexing cancelled");
        }

        let entry = entry.with_context(|| {
            format!(
                "failed to walk index path under {}",
                scope.target.to_string_lossy()
            )
        })?;
        let path = entry.path();
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }

        match file_candidate(&scope.workspace, path)? {
            Some(candidate) => files.push(candidate),
            None => skipped_files = skipped_files.saturating_add(1),
        }

        if files.len() >= options.limit {
            truncated = true;
            break;
        }
    }
    skipped_files = skipped_files.saturating_add(pruned_entries.load(Ordering::Relaxed));

    Ok(DiscoveryResult {
        scope,
        files,
        skipped_files,
        truncated,
        timed_out,
    })
}

pub(crate) fn storage_relative_path(workspace: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(workspace)
        .with_context(|| format!("{} is outside {}", path.display(), workspace.display()))?;
    if relative.as_os_str().is_empty() {
        return Ok(".".to_string());
    }

    let mut normalized = String::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                if !normalized.is_empty() {
                    normalized.push('/');
                }
                normalized.push_str(&part.to_string_lossy());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                bail!(
                    "path contains unsupported component: {}",
                    relative.display()
                );
            }
        }
    }
    Ok(normalized)
}

pub(crate) fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn resolve_existing_under_workspace(input: &Path, workspace: &Path) -> Result<PathBuf> {
    if input.as_os_str().is_empty() {
        bail!("path is required");
    }

    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        workspace.join(input)
    };
    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve path {}", input.display()))?;

    if resolved == workspace || resolved.starts_with(workspace) {
        Ok(resolved)
    } else {
        Err(anyhow!(
            "path {} resolves outside the server working directory {}",
            input.display(),
            workspace.display()
        ))
    }
}

fn file_candidate(workspace: &Path, path: &Path) -> Result<Option<FileCandidate>> {
    let Some(language) = language_for_path(path) else {
        return Ok(None);
    };

    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.len() > MAX_FILE_BYTES {
        return Ok(None);
    }

    Ok(Some(FileCandidate {
        absolute_path: path.to_path_buf(),
        relative_path: storage_relative_path(workspace, path)?,
        language: language.to_string(),
    }))
}

fn should_skip_path(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::Normal(name) if is_excluded_name(name)))
}

fn language_for_path(path: &Path) -> Option<&'static str> {
    language_for_extension(path.extension()?.to_str()?)
}

fn is_excluded_name(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | ".svn" | ".hg" | "target" | "node_modules" | "dist" | "build" | ".tools-mcp")
    )
}

fn language_for_extension(extension: &str) -> Option<&'static str> {
    if extension.eq_ignore_ascii_case("rs") {
        Some("rust")
    } else if extension.eq_ignore_ascii_case("ts") {
        Some("typescript")
    } else if extension.eq_ignore_ascii_case("tsx") {
        Some("tsx")
    } else if extension.eq_ignore_ascii_case("js")
        || extension.eq_ignore_ascii_case("mjs")
        || extension.eq_ignore_ascii_case("cjs")
        || extension.eq_ignore_ascii_case("jsx")
    {
        Some("javascript")
    } else if extension.eq_ignore_ascii_case("py") || extension.eq_ignore_ascii_case("pyi") {
        Some("python")
    } else if extension.eq_ignore_ascii_case("go") {
        Some("go")
    } else if extension.eq_ignore_ascii_case("cpp")
        || extension.eq_ignore_ascii_case("cxx")
        || extension.eq_ignore_ascii_case("cc")
        || extension.eq_ignore_ascii_case("h")
        || extension.eq_ignore_ascii_case("hpp")
        || extension.eq_ignore_ascii_case("hxx")
    {
        Some("cpp")
    } else if extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown") {
        Some("markdown")
    } else if extension.eq_ignore_ascii_case("toml") {
        Some("toml")
    } else if extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml") {
        Some("yaml")
    } else if extension.eq_ignore_ascii_case("json") {
        Some("json")
    } else if extension.eq_ignore_ascii_case("txt") {
        Some("text")
    } else {
        None
    }
}

pub(crate) fn discovered_path_set(files: &[FileCandidate]) -> HashSet<&str> {
    let mut paths = HashSet::with_capacity(files.len());
    paths.extend(files.iter().map(|file| file.relative_path.as_str()));
    paths
}

#[cfg(test)]
mod tests {
    use super::{PathFilter, escape_sql_literal, language_for_path, should_skip_path};
    use std::path::Path;

    #[test]
    fn directory_filter_includes_children_only() {
        let filter = PathFilter::Directory("src/app".to_string());

        assert!(filter.contains("src/app"));
        assert!(filter.contains("src/app/main.rs"));
        assert!(!filter.contains("src/application/main.rs"));
    }

    #[test]
    fn sql_literals_escape_single_quotes() {
        assert_eq!(escape_sql_literal("src/it's.rs"), "src/it''s.rs");
    }

    #[test]
    fn directory_filter_sql_treats_wildcards_literally() {
        let filter = PathFilter::Directory("smart_file.edit".to_string());

        assert_eq!(
            filter.to_sql().as_deref(),
            Some(
                "(path = 'smart_file.edit' OR (path >= 'smart_file.edit/' AND path < 'smart_file.edit0'))"
            )
        );
    }

    #[test]
    fn language_detection_is_ascii_case_insensitive() {
        assert_eq!(language_for_path(Path::new("src/lib.RS")), Some("rust"));
        assert_eq!(language_for_path(Path::new("src/app.TSX")), Some("tsx"));
        assert_eq!(
            language_for_path(Path::new("types/model.PYI")),
            Some("python")
        );
        assert_eq!(language_for_path(Path::new("archive.tar.gz")), None);
    }

    #[test]
    fn skip_path_matches_complete_components_only() {
        assert!(should_skip_path(
            &Path::new("src")
                .join("node_modules")
                .join("pkg")
                .join("index.ts")
        ));
        assert!(should_skip_path(
            &Path::new(".tools-mcp").join("semantic-index")
        ));
        assert!(!should_skip_path(&Path::new("src").join("node_modules.rs")));
        assert!(!should_skip_path(
            &Path::new("src").join("distillery").join("main.rs")
        ));
    }
}
