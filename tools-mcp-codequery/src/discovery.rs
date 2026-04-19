//! Workspace and file discovery for `CodeQuery`.
//!
//! When `file_paths` is not supplied, `CodeQuery` walks the workspace tree (rooted at the
//! enclosing git top-level when available) using the `ignore` crate so `.gitignore` and
//! related rules are honored. Common build/dependency directories are skipped on top of
//! that, and the file extension filter from `openai-file-search-core` decides what is
//! actually indexable.
//!
//! [`default_workspace_scope`] also computes a stable cache key + default vector-store
//! name from the resolved root, so each checkout gets a consistent store without
//! requiring extra configuration.

use anyhow::{Result, anyhow};
use ignore::{DirEntry, WalkBuilder};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceScope {
    pub(crate) root: PathBuf,
    pub(crate) cache_key: String,
    pub(crate) default_store_name: String,
}

fn discover_workspace_root_from(start: &Path) -> Result<PathBuf> {
    let start = start.canonicalize()?;
    for ancestor in start.ancestors() {
        if ancestor.join(".git").exists() {
            return Ok(ancestor.to_path_buf());
        }
    }
    Ok(start)
}

fn workspace_fingerprint(root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(root.as_os_str().to_string_lossy().as_bytes());
    hex::encode(hasher.finalize())
}

fn default_workspace_scope_from(start: &Path) -> Result<WorkspaceScope> {
    let root = discover_workspace_root_from(start)?;
    let base_name = root
        .file_name()
        .and_then(|os| os.to_str())
        .map(std::string::ToString::to_string)
        .ok_or_else(|| anyhow!("workspace root {} has no usable name", root.display()))?;
    let fingerprint = workspace_fingerprint(&root);
    let short = &fingerprint[..8];
    Ok(WorkspaceScope {
        root,
        cache_key: format!("auto::{fingerprint}"),
        default_store_name: format!("{base_name} [{short}]"),
    })
}

pub(crate) fn default_workspace_scope() -> Result<WorkspaceScope> {
    let cwd = std::env::current_dir()?;
    default_workspace_scope_from(&cwd)
}

/// Directories to skip during file discovery on top of `.gitignore` rules.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".idea",
    ".vscode",
    ".venv",
    "__pycache__",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "coverage",
    "tmp",
];

fn discover_default_file_paths_from(
    start: &Path,
    root_override: Option<&Path>,
) -> Result<Vec<String>> {
    let root = match root_override {
        Some(root) => root.to_path_buf(),
        None => discover_workspace_root_from(start)?,
    };
    let mut results = Vec::new();

    // Use `ignore`'s walker so `.gitignore` (plus global/exclude rules) are respected by default.
    // We layer our own `should_visit`/`should_index_file` policy on top.
    for entry in WalkBuilder::new(&root)
        .follow_links(false)
        // We apply our own dotfile/dotdir policy rather than `ignore`'s hidden-file default.
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        // Treat `.gitignore` as authoritative even when the checkout isn't a full git repo
        // (e.g. vendored source tree, exported zip, CI artifact).
        .require_git(false)
        .parents(true)
        .filter_entry(should_visit)
        .build()
    {
        let entry = entry?;
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let path = entry.path();
        if should_index_file(path) {
            results.push(path.to_string_lossy().to_string());
        }
    }

    if results.is_empty() {
        return Err(anyhow!("No indexable files found under {}", root.display()));
    }

    results.sort();
    Ok(results)
}

pub(crate) fn discover_default_file_paths(root_override: Option<&Path>) -> Result<Vec<String>> {
    let cwd = std::env::current_dir()?;
    discover_default_file_paths_from(&cwd, root_override)
}

fn should_visit(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if let Some(name) = entry.file_name().to_str()
        && entry
            .file_type()
            .is_some_and(|file_type| file_type.is_dir())
    {
        let lower = name.to_ascii_lowercase();
        if SKIP_DIRS.contains(&lower.as_str()) {
            return false;
        }
        if lower.starts_with('.') {
            return false;
        }
    }

    true
}

fn should_index_file(path: &Path) -> bool {
    openai_file_search_core::is_codequery_indexable_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discover_default_file_paths_respects_gitignore() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("Cargo.toml"), b"[package]\nname = \"x\"\n").unwrap();
        fs::write(temp.path().join("ignored.rs"), b"fn ignored() {}\n").unwrap();
        fs::write(temp.path().join("kept.rs"), b"fn kept() {}\n").unwrap();
        fs::write(temp.path().join("README.md"), b"# docs\n").unwrap();
        fs::write(temp.path().join("logo.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        fs::write(temp.path().join(".gitignore"), b"ignored.rs\n").unwrap();

        let discovered = discover_default_file_paths_from(temp.path(), None).unwrap();

        let discovered_joined = discovered.join("\n");
        assert!(discovered_joined.contains("kept.rs"));
        assert!(!discovered_joined.contains("ignored.rs"));
        assert!(!discovered_joined.contains("README.md"));
        assert!(!discovered_joined.contains("logo.png"));
    }

    #[test]
    fn default_workspace_scope_uses_git_top_level() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp.path())
            .status()
            .expect("git init")
            .success()
            .then_some(())
            .expect("git init should succeed");
        fs::create_dir_all(temp.path().join("nested/deeper")).expect("nested dirs");

        let nested = temp.path().join("nested/deeper");
        let scope = default_workspace_scope_from(&nested).expect("scope");

        assert_eq!(
            scope.root,
            temp.path().canonicalize().expect("canonical root")
        );
        assert!(scope.default_store_name.contains('['));
        assert!(scope.default_store_name.contains(']'));
    }

    #[test]
    fn discover_default_file_paths_walks_git_top_level() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(temp.path())
            .status()
            .expect("git init")
            .success()
            .then_some(())
            .expect("git init should succeed");
        fs::write(temp.path().join("root.rs"), b"fn root_level() {}\n").expect("root file");
        fs::create_dir_all(temp.path().join("nested/deeper")).expect("nested dirs");

        let nested = temp.path().join("nested/deeper");
        let discovered = discover_default_file_paths_from(&nested, None).expect("discover");

        let discovered_joined = discovered.join("\n");
        assert!(discovered_joined.contains("root.rs"));
    }
}
