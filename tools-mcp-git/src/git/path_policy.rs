use std::path::{Component, Path, PathBuf};

fn authority_root() -> Result<PathBuf, String> {
    std::env::current_dir()
        .map_err(|err| format!("failed to resolve server working directory: {err}"))?
        .canonicalize()
        .map_err(|err| format!("failed to canonicalize server working directory: {err}"))
}

fn absolute_path(path: &Path, root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn path_exists_or_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn deepest_existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|ancestor| path_exists_or_symlink(ancestor))
}

fn ensure_inside_authority(
    field_name: &str,
    original: &Path,
    canonical_path: &Path,
    root: &Path,
) -> Result<(), String> {
    if canonical_path == root || canonical_path.starts_with(root) {
        Ok(())
    } else {
        Err(format!(
            "{field_name} must resolve inside the server working directory ({}): {} resolves outside the permitted authority",
            root.display(),
            original.display()
        ))
    }
}

fn canonicalize_existing(field_name: &str, path: &Path) -> Result<PathBuf, String> {
    path.canonicalize().map_err(|err| {
        format!(
            "failed to canonicalize existing path component for {field_name} ({}): {err}",
            path.display()
        )
    })
}

fn append_uncreated_tail(
    field_name: &str,
    mut current: PathBuf,
    tail: &Path,
    original: &Path,
    root: &Path,
) -> Result<PathBuf, String> {
    for component in tail.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !current.pop() {
                    return Err(format!(
                        "{field_name} resolves above the filesystem root: {}",
                        original.display()
                    ));
                }
                ensure_inside_authority(field_name, original, &current, root)?;
            }
            Component::Normal(name) => {
                current.push(name);
                if path_exists_or_symlink(&current) {
                    let canonical = canonicalize_existing(field_name, &current)?;
                    ensure_inside_authority(field_name, original, &canonical, root)?;
                    current = canonical;
                } else {
                    ensure_inside_authority(field_name, original, &current, root)?;
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(format!(
                    "{field_name} contains an unexpected rooted path segment: {}",
                    original.display()
                ));
            }
        }
    }

    ensure_inside_authority(field_name, original, &current, root)?;
    Ok(current)
}

fn resolve_under_authority(
    field_name: &str,
    path: &Path,
    root: &Path,
    require_existing_final: bool,
) -> Result<PathBuf, String> {
    if path_exists_or_symlink(path) {
        let canonical_path = canonicalize_existing(field_name, path)?;
        ensure_inside_authority(field_name, path, &canonical_path, root)?;
        return Ok(canonical_path);
    }

    if require_existing_final {
        return Err(format!(
            "{field_name} must reference an existing directory: {} does not exist",
            path.display()
        ));
    }

    let ancestor = deepest_existing_ancestor(path).ok_or_else(|| {
        format!("{field_name} has no existing ancestor inside the server working directory")
    })?;
    let canonical_ancestor = canonicalize_existing(field_name, ancestor)?;
    ensure_inside_authority(field_name, path, &canonical_ancestor, root)?;

    let tail = path.strip_prefix(ancestor).map_err(|err| {
        format!(
            "failed to resolve remaining path after existing ancestor for {field_name} ({}): {err}",
            path.display()
        )
    })?;

    append_uncreated_tail(field_name, canonical_ancestor, tail, path, root)
}

fn require_non_empty(field_name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field_name} must be non-empty when provided"))
    } else {
        Ok(())
    }
}

fn validate_path(
    field_name: &str,
    value: &str,
    require_existing_final: bool,
) -> Result<PathBuf, String> {
    require_non_empty(field_name, value)?;

    let root = authority_root()?;
    let path = absolute_path(Path::new(value), &root);
    resolve_under_authority(field_name, &path, &root, require_existing_final)
}

fn resolve_existing_directory(field_name: &str, value: &str) -> Result<PathBuf, String> {
    let resolved = validate_path(field_name, value, true)?;
    let metadata = std::fs::metadata(&resolved)
        .map_err(|err| format!("{field_name} must reference an existing directory: {err}"))?;
    if !metadata.is_dir() {
        return Err(format!(
            "{field_name} must reference an existing directory: {} is not a directory",
            resolved.display()
        ));
    }
    Ok(resolved)
}

pub(crate) fn resolve_working_dir(working_dir: Option<&str>) -> Result<Option<PathBuf>, String> {
    let Some(working_dir) = working_dir else {
        return Ok(None);
    };
    resolve_existing_directory("working_dir", working_dir).map(Some)
}

pub(crate) fn resolve_output_dir(output_dir: &str) -> Result<PathBuf, String> {
    validate_path("output_dir", output_dir, false)
}

#[cfg(test)]
mod tests {
    use super::{resolve_output_dir, resolve_working_dir};
    #[cfg(unix)]
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/tools-mcp-git-path-policy-tests")
            .join(format!("{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn allows_current_working_dir() {
        let cwd = std::env::current_dir().expect("current dir");
        resolve_working_dir(Some(cwd.to_string_lossy().as_ref()))
            .expect("current working directory should be in scope");
    }

    #[test]
    fn rejects_parent_working_dir() {
        let cwd = std::env::current_dir().expect("current dir");
        let parent = cwd.parent().expect("current dir should have parent");
        let err = resolve_working_dir(Some(parent.to_string_lossy().as_ref()))
            .expect_err("parent directory should be out of scope");
        assert!(err.contains("working_dir must resolve inside"));
    }

    #[test]
    fn allows_nonexistent_output_dir_under_current_working_dir() {
        let output_dir = std::env::current_dir()
            .expect("current dir")
            .join("target")
            .join("path-policy-nonexistent-output")
            .join(std::process::id().to_string());
        resolve_output_dir(output_dir.to_string_lossy().as_ref())
            .expect("nonexistent output directory under cwd should be in scope");
    }

    #[test]
    fn rejects_nonexistent_output_dir_under_parent() {
        let cwd = std::env::current_dir().expect("current dir");
        let parent = cwd.parent().expect("current dir should have parent");
        let output_dir = parent
            .join("path-policy-out-of-scope-output")
            .join(std::process::id().to_string());
        let err = resolve_output_dir(output_dir.to_string_lossy().as_ref())
            .expect_err("output dir under parent should be out of scope");
        assert!(err.contains("output_dir must resolve inside"));
    }

    #[test]
    fn rejects_output_dir_that_escapes_after_uncreated_tail() {
        let output_dir = std::env::current_dir()
            .expect("current dir")
            .join("target")
            .join("path-policy-new-output")
            .join("..")
            .join("..")
            .join("..")
            .join(format!("out-of-scope-{}", std::process::id()));
        let err = resolve_output_dir(output_dir.to_string_lossy().as_ref())
            .expect_err("output dir escaping through uncreated tail should be out of scope");
        assert!(err.contains("output_dir must resolve inside"));
    }

    #[cfg(unix)]
    #[test]
    fn working_dir_resolution_returns_canonical_symlink_target() {
        use std::os::unix::fs as unix_fs;

        let root = unique_test_dir("working-dir-symlink");
        let target = root.join("target");
        let link = root.join("link");
        std::fs::create_dir_all(&target).expect("create target dir");
        unix_fs::symlink(&target, &link).expect("symlink target");

        let resolved = resolve_working_dir(Some(link.to_string_lossy().as_ref()))
            .expect("symlinked working dir should resolve")
            .expect("working dir should be present");

        assert_eq!(resolved, target.canonicalize().expect("canonical target"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn output_dir_resolution_canonicalizes_symlinked_parent() {
        use std::os::unix::fs as unix_fs;

        let root = unique_test_dir("output-dir-symlink");
        let target = root.join("target");
        let link = root.join("link");
        std::fs::create_dir_all(&target).expect("create target dir");
        unix_fs::symlink(&target, &link).expect("symlink target");

        let resolved = resolve_output_dir(link.join("patches").to_string_lossy().as_ref())
            .expect("output dir under symlinked parent should resolve");

        assert_eq!(
            resolved,
            target
                .canonicalize()
                .expect("canonical target")
                .join("patches")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
