use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub(crate) struct PathPolicyError {
    field: &'static str,
    workspace: PathBuf,
    reason: String,
}

impl PathPolicyError {
    fn new(
        field: &'static str,
        _path: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            field,
            workspace: workspace.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for PathPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "path rejected for '{}': {}. The resolved path must stay inside the server working directory {}. Remediation: use a relative path under the server working directory, or an absolute path within that directory.",
            self.field,
            self.reason,
            self.workspace.display()
        )
    }
}

pub(crate) fn resolve_mutation_path(
    input: impl AsRef<Path>,
    field: &'static str,
) -> Result<PathBuf, PathPolicyError> {
    let workspace = canonical_workspace(field, input.as_ref())?;
    let absolute = absolute_candidate(input.as_ref(), &workspace, field)?;
    resolve_under_workspace(&absolute, &workspace, field, false)
}

pub(crate) fn resolve_existing_directory(
    input: impl AsRef<Path>,
    field: &'static str,
) -> Result<PathBuf, PathPolicyError> {
    let workspace = canonical_workspace(field, input.as_ref())?;
    let absolute = absolute_candidate(input.as_ref(), &workspace, field)?;
    let resolved = resolve_existing_under_workspace(&absolute, &workspace, field)?;
    if !resolved.is_dir() {
        return Err(PathPolicyError::new(
            field,
            input.as_ref(),
            workspace,
            format!("{} is not an existing directory", input.as_ref().display()),
        ));
    }
    Ok(resolved)
}

pub(crate) fn resolve_existing_file(
    input: impl AsRef<Path>,
    field: &'static str,
) -> Result<PathBuf, PathPolicyError> {
    let workspace = canonical_workspace(field, input.as_ref())?;
    let absolute = absolute_candidate(input.as_ref(), &workspace, field)?;
    let resolved = resolve_existing_under_workspace(&absolute, &workspace, field)?;
    if !resolved.is_file() {
        return Err(PathPolicyError::new(
            field,
            input.as_ref(),
            workspace,
            format!("{} is not an existing file", input.as_ref().display()),
        ));
    }
    Ok(resolved)
}

fn canonical_workspace(field: &'static str, input: &Path) -> Result<PathBuf, PathPolicyError> {
    let cwd = std::env::current_dir().map_err(|err| {
        PathPolicyError::new(
            field,
            input,
            PathBuf::from("."),
            format!("failed to read server working directory: {err}"),
        )
    })?;
    cwd.canonicalize().map_err(|err| {
        PathPolicyError::new(
            field,
            input,
            cwd,
            format!("failed to resolve server working directory: {err}"),
        )
    })
}

fn absolute_candidate(
    input: &Path,
    workspace: &Path,
    field: &'static str,
) -> Result<PathBuf, PathPolicyError> {
    if input.as_os_str().is_empty() {
        return Err(PathPolicyError::new(
            field,
            input,
            workspace,
            "path is empty",
        ));
    }

    if input.is_absolute() {
        return Ok(input.to_path_buf());
    }

    if input
        .components()
        .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(PathPolicyError::new(
            field,
            input,
            workspace,
            format!(
                "{} is rooted but not absolute; use a path under the server working directory",
                input.display()
            ),
        ));
    }

    Ok(workspace.join(input))
}

fn resolve_under_workspace(
    absolute: &Path,
    workspace: &Path,
    field: &'static str,
    require_existing_final: bool,
) -> Result<PathBuf, PathPolicyError> {
    if path_exists_or_symlink(absolute) {
        return resolve_existing_mutation_path(absolute, workspace, field);
    }

    if require_existing_final {
        return Err(PathPolicyError::new(
            field,
            absolute,
            workspace,
            format!("{} does not exist", absolute.display()),
        ));
    }

    let ancestor = deepest_existing_ancestor(absolute).ok_or_else(|| {
        PathPolicyError::new(
            field,
            absolute,
            workspace,
            format!(
                "could not find an existing ancestor for {}",
                absolute.display()
            ),
        )
    })?;
    let canonical_ancestor = canonicalize_checked(ancestor, workspace, field)?;
    ensure_inside_workspace(&canonical_ancestor, ancestor, workspace, field)?;

    let rest = absolute.strip_prefix(ancestor).map_err(|err| {
        PathPolicyError::new(
            field,
            absolute,
            workspace,
            format!("failed to resolve remaining path after existing ancestor: {err}"),
        )
    })?;

    append_uncreated_tail(canonical_ancestor, rest, absolute, workspace, field)
}

fn resolve_existing_under_workspace(
    absolute: &Path,
    workspace: &Path,
    field: &'static str,
) -> Result<PathBuf, PathPolicyError> {
    if !path_exists_or_symlink(absolute) {
        return Err(PathPolicyError::new(
            field,
            absolute,
            workspace,
            format!("{} does not exist", absolute.display()),
        ));
    }

    let canonical = canonicalize_checked(absolute, workspace, field)?;
    ensure_inside_workspace(&canonical, absolute, workspace, field)?;
    Ok(canonical)
}

fn resolve_existing_mutation_path(
    absolute: &Path,
    workspace: &Path,
    field: &'static str,
) -> Result<PathBuf, PathPolicyError> {
    let canonical = resolve_existing_under_workspace(absolute, workspace, field)?;
    let metadata = std::fs::symlink_metadata(absolute).map_err(|err| {
        PathPolicyError::new(
            field,
            absolute,
            workspace,
            format!(
                "failed to inspect {} after resolving path: {err}",
                absolute.display()
            ),
        )
    })?;

    if metadata.file_type().is_symlink() {
        Ok(absolute.to_path_buf())
    } else {
        Ok(canonical)
    }
}

fn append_uncreated_tail(
    mut current: PathBuf,
    tail: &Path,
    original: &Path,
    workspace: &Path,
    field: &'static str,
) -> Result<PathBuf, PathPolicyError> {
    for component in tail.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !current.pop() {
                    return Err(PathPolicyError::new(
                        field,
                        original,
                        workspace,
                        format!("{} resolves above the filesystem root", original.display()),
                    ));
                }
                ensure_inside_workspace(&current, original, workspace, field)?;
            }
            Component::Normal(name) => {
                current.push(name);
                if path_exists_or_symlink(&current) {
                    let canonical = canonicalize_checked(&current, workspace, field)?;
                    ensure_inside_workspace(&canonical, original, workspace, field)?;
                    current = canonical;
                } else {
                    ensure_inside_workspace(&current, original, workspace, field)?;
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(PathPolicyError::new(
                    field,
                    original,
                    workspace,
                    format!(
                        "{} contains an unexpected rooted path segment",
                        original.display()
                    ),
                ));
            }
        }
    }

    ensure_inside_workspace(&current, original, workspace, field)?;
    Ok(current)
}

fn deepest_existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|ancestor| path_exists_or_symlink(ancestor))
}

fn path_exists_or_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn canonicalize_checked(
    path: &Path,
    workspace: &Path,
    field: &'static str,
) -> Result<PathBuf, PathPolicyError> {
    path.canonicalize().map_err(|err| {
        PathPolicyError::new(
            field,
            path,
            workspace,
            format!(
                "failed to resolve {} through existing path components: {err}",
                path.display()
            ),
        )
    })
}

fn ensure_inside_workspace(
    resolved: &Path,
    original: &Path,
    workspace: &Path,
    field: &'static str,
) -> Result<(), PathPolicyError> {
    if resolved == workspace || resolved.starts_with(workspace) {
        return Ok(());
    }

    Err(PathPolicyError::new(
        field,
        original,
        workspace,
        format!(
            "{} resolves outside the server working directory",
            original.display()
        ),
    ))
}

#[cfg(test)]
pub(crate) fn tempdir_in_workspace(prefix: &str) -> tempfile::TempDir {
    let root = std::env::current_dir()
        .expect("current dir")
        .join("target")
        .join("local-path-policy-tests");
    std::fs::create_dir_all(&root).expect("create local test temp root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("tempdir in workspace")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_non_existing_path_inside_workspace() {
        let path = resolve_mutation_path("target/local-policy-new/file.txt", "path")
            .expect("in-workspace path should resolve");

        assert!(path.is_absolute());
        assert!(
            path.ends_with(
                Path::new("target")
                    .join("local-policy-new")
                    .join("file.txt")
            )
        );
    }

    #[test]
    fn rejects_parent_traversal_outside_workspace() {
        let err = resolve_mutation_path("..", "path").expect_err("parent escape should fail");

        assert!(
            err.to_string()
                .contains("outside the server working directory")
        );
    }

    #[test]
    fn permits_parent_traversal_that_stays_inside_workspace() {
        let path = resolve_mutation_path("target/../Cargo.toml", "path")
            .expect("resolved path stays in workspace");

        assert!(path.ends_with("Cargo.toml"));
    }

    #[test]
    fn rejects_absolute_path_outside_workspace() {
        let workspace = std::env::current_dir().expect("current dir");
        let outside = workspace
            .parent()
            .expect("workspace parent")
            .join("definitely-outside-tools-mcp-local-policy");

        let err = resolve_mutation_path(&outside, "path").expect_err("outside path should fail");

        assert!(
            err.to_string()
                .contains("outside the server working directory")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_existing_symlink_that_resolves_outside_workspace() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir_in_workspace("symlink-outside-");
        let link = dir.path().join("link-out");
        let outside = std::env::current_dir()
            .expect("current dir")
            .parent()
            .expect("workspace parent")
            .to_path_buf();
        unix_fs::symlink(&outside, &link).expect("symlink to workspace parent");

        let err = resolve_mutation_path(&link, "path").expect_err("symlink escape should fail");
        let message = err.to_string();

        assert!(message.contains("outside the server working directory"));
        assert!(
            !message.contains(&outside.display().to_string()),
            "rejection should not expose the symlink target: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonicalizes_existing_file_reached_through_symlinked_parent() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir_in_workspace("symlink-parent-file-");
        let target_dir = dir.path().join("target");
        let link_dir = dir.path().join("link");
        std::fs::create_dir_all(&target_dir).expect("create target dir");
        let target_file = target_dir.join("file.txt");
        std::fs::write(&target_file, "content").expect("write target file");
        unix_fs::symlink(&target_dir, &link_dir).expect("symlink to target dir");

        let resolved = resolve_mutation_path(link_dir.join("file.txt"), "path")
            .expect("symlinked parent inside workspace should resolve");

        assert_eq!(
            resolved,
            target_file.canonicalize().expect("canonical target")
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_directory_resolution_returns_canonical_symlink_target() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir_in_workspace("symlink-dir-");
        let target_dir = dir.path().join("target");
        let link_dir = dir.path().join("link");
        std::fs::create_dir_all(&target_dir).expect("create target dir");
        unix_fs::symlink(&target_dir, &link_dir).expect("symlink to target dir");

        let resolved = resolve_existing_directory(&link_dir, "working_dir")
            .expect("symlinked directory inside workspace should resolve");

        assert_eq!(
            resolved,
            target_dir.canonicalize().expect("canonical target")
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_file_resolution_returns_canonical_symlink_target() {
        use std::os::unix::fs as unix_fs;

        let dir = tempdir_in_workspace("symlink-file-");
        let target_file = dir.path().join("target.txt");
        let link_file = dir.path().join("link.txt");
        std::fs::write(&target_file, "content").expect("write target file");
        unix_fs::symlink(&target_file, &link_file).expect("symlink to target file");

        let resolved = resolve_existing_file(&link_file, "path")
            .expect("symlinked file inside workspace should resolve");

        assert_eq!(
            resolved,
            target_file.canonicalize().expect("canonical target")
        );
    }
}
