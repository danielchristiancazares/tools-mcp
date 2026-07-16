use super::scope_cache::{RepoScopeKey, ScopeCacheError, ScopeFileType, repo_scope_cache};
use glob::{MatchOptions, Pattern};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::config::{DEFAULT_GLOB_LIMIT, MAX_GLOB_LIMIT};
use tools_mcp_core::define_mcp_tool;
use tools_mcp_core::validation;

// Glob has no per-call timeout today; pick a generous default consistent with
// the previous in-process `ignore::WalkBuilder` walk, which was unbounded.
const GLOB_SCOPE_CACHE_DEADLINE: Duration = Duration::from_secs(10);

const MAX_BRACE_ALTERNATIVES: usize = 64;
const MAX_EXPANDED_PATTERNS: usize = 1024;

/// Expands brace patterns like `{a,b,c}` into multiple alternatives.
/// Handles nested braces and multiple brace groups.
/// Example: `**/*.{cpp,h}` -> `["**/*.cpp", "**/*.h"]`
fn expand_braces(pattern: &str) -> Result<Vec<String>, String> {
    let mut results = vec![pattern.to_string()];

    loop {
        let mut expanded = false;
        let mut new_results = Vec::new();

        for pat in &results {
            if let Some(expansion) = expand_single_brace(pat)? {
                new_results.extend(expansion);
                expanded = true;
            } else {
                new_results.push(pat.clone());
            }

            if new_results.len() > MAX_EXPANDED_PATTERNS {
                return Err(format!(
                    "brace expansion exceeded maximum of {MAX_EXPANDED_PATTERNS} patterns"
                ));
            }
        }

        results = new_results;
        if !expanded {
            break;
        }
    }

    Ok(results)
}

/// Expands the first (innermost) brace group found in the pattern.
/// Returns None if no braces found.
fn expand_single_brace(pattern: &str) -> Result<Option<Vec<String>>, String> {
    // Find innermost brace group (one without nested braces)
    let bytes = pattern.as_bytes();
    let mut brace_stack = Vec::new();

    for (i, &b) in bytes.iter().enumerate() {
        if b == b'{' {
            brace_stack.push(i);
        } else if b == b'}'
            && let Some(start) = brace_stack.pop()
        {
            // Found a complete brace group from start to i
            let prefix = &pattern[..start];
            let suffix = &pattern[i + 1..];
            let alternatives = &pattern[start + 1..i];

            // Split by comma (handling escaped commas would be complex, skip for now)
            let parts: Vec<&str> = alternatives.split(',').collect();
            if parts.len() > MAX_BRACE_ALTERNATIVES {
                return Err(format!(
                    "brace group exceeded maximum of {MAX_BRACE_ALTERNATIVES} alternatives"
                ));
            }

            if parts.len() > 1 {
                return Ok(Some(
                    parts
                        .into_iter()
                        .map(|p| format!("{prefix}{p}{suffix}"))
                        .collect(),
                ));
            }
            // Single-item braces are literals, not brace expansion.
        }
    }

    Ok(None)
}

/// Returns the literal tail of `pattern` after the last glob metacharacter,
/// usable as a cheap `ends_with` prefilter before full glob matching.
///
/// Returns `None` (no prefilter) unless the tail is non-empty, contains no
/// path separators (glob matching treats `/` and `\` as equivalent, which a
/// byte comparison would not), and matching is case-sensitive. `]` is treated
/// as a metacharacter so a tail is never taken from inside a `[...]` class.
fn required_literal_suffix(pattern: &str) -> Option<&str> {
    let tail_start = pattern
        .rfind(['*', '?', '[', ']'])
        .map_or(0, |index| index + 1);
    let suffix = &pattern[tail_start..];

    if suffix.is_empty() || suffix.contains(is_glob_separator) {
        return None;
    }

    Some(suffix)
}

fn glob_traversal_max_depth(patterns: &[String]) -> Option<usize> {
    let mut max_depth = 0;

    for pattern in patterns {
        let depth = pattern_traversal_max_depth(pattern)?;
        max_depth = max_depth.max(depth);
    }

    Some(max_depth)
}

fn pattern_traversal_max_depth(pattern: &str) -> Option<usize> {
    let mut depth = 0;
    let mut component_len = 0;
    let mut component_is_recursive = false;
    let mut in_class = false;

    for ch in pattern.chars() {
        if !in_class && is_glob_separator(ch) {
            finish_depth_component(&mut depth, &mut component_len, &mut component_is_recursive)?;
            continue;
        }

        match ch {
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            _ => {}
        }

        match component_len {
            0 => component_is_recursive = ch == '*',
            1 => component_is_recursive = component_is_recursive && ch == '*',
            _ => component_is_recursive = false,
        }
        component_len += 1;
    }

    finish_depth_component(&mut depth, &mut component_len, &mut component_is_recursive)?;
    Some(depth)
}

fn finish_depth_component(
    depth: &mut usize,
    component_len: &mut usize,
    component_is_recursive: &mut bool,
) -> Option<()> {
    if *component_len == 0 {
        return Some(());
    }

    if *component_len == 2 && *component_is_recursive {
        return None;
    }

    *depth += 1;
    *component_len = 0;
    *component_is_recursive = false;
    Some(())
}

fn is_glob_separator(ch: char) -> bool {
    ch == '/' || ch == '\\'
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobRequest {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    hidden: Option<bool>,
    #[serde(default)]
    limit: Option<usize>,
}

#[allow(clippy::unused_async)]
async fn handle_glob(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<GlobRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    if let Err(o) = validation::validate_non_empty(&req.pattern, "pattern", None) {
        return o;
    }

    let base_path = req.path.as_deref().unwrap_or(".");
    let include_hidden = req.hidden.unwrap_or(false);
    let limit = validation::clamp_limit(req.limit, DEFAULT_GLOB_LIMIT, 1, MAX_GLOB_LIMIT);

    let base = Path::new(base_path);
    if !base.exists() {
        return ToolCallOutcome::err(format!(
            "base path does not exist: {}. Remediation: set 'path' to an existing directory (or omit it to use '.').",
            base.display()
        ));
    }
    if !base.is_dir() {
        return ToolCallOutcome::err(format!(
            "base path is not a directory: {}. Remediation: pass a directory path to 'path'.",
            base.display()
        ));
    }

    // Expand brace patterns and parse each
    let expanded = match expand_braces(&req.pattern) {
        Ok(expanded) => expanded,
        Err(err) => {
            return ToolCallOutcome::err(format!(
                "invalid glob pattern: {err}. Remediation: reduce brace groups/options or use a simpler pattern."
            ));
        }
    };
    let patterns: Vec<Pattern> = match expanded
        .iter()
        .map(|p| Pattern::new(p))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(ps) => ps,
        Err(err) => {
            return ToolCallOutcome::err(format!(
                "invalid glob pattern: {err}. Remediation: use patterns like '**/*.rs' or 'src/*.{{ts,tsx}}'."
            ));
        }
    };

    let match_options = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: !include_hidden,
    };

    // Reuse the shared recursive-scope snapshot for this root + flag set so
    // repeat globs on the same scope skip an `ignore::WalkBuilder` traversal.
    // Finite non-`**` patterns can bound traversal depth without changing
    // which relative paths can match under `require_literal_separator`.
    let key = RepoScopeKey {
        root: base.to_path_buf(),
        hidden: include_hidden,
        follow: false,
        no_ignore: false,
        max_depth: glob_traversal_max_depth(&expanded),
    };
    let deadline = Instant::now() + GLOB_SCOPE_CACHE_DEADLINE;
    let snapshot = match repo_scope_cache().get_or_build(&key, deadline) {
        Ok(snapshot) => snapshot,
        Err(ScopeCacheError::Walk(message)) => {
            return ToolCallOutcome::err(format!(
                "glob walk error: {message}. Remediation: check directory permissions or try a narrower 'path'."
            ));
        }
        Err(ScopeCacheError::Io(err)) => {
            return ToolCallOutcome::err(format!(
                "glob: I/O error: {err}. Remediation: check directory permissions or try a narrower 'path'."
            ));
        }
        Err(ScopeCacheError::Timeout) => {
            return ToolCallOutcome::err(
                "glob: scope walk timed out. Remediation: narrow 'path' or reduce the search scope.".to_string(),
            );
        }
    };

    // Precompute a required literal suffix per pattern (e.g. ".rs" for "**/*.rs")
    // so most non-matching entries are rejected with one byte comparison instead
    // of a full backtracking glob match.
    let pattern_suffixes: Vec<Option<&str>> = expanded
        .iter()
        .map(|pattern| {
            match_options
                .case_sensitive
                .then(|| required_literal_suffix(pattern))
                .flatten()
        })
        .collect();

    let mut files: Vec<String> = Vec::with_capacity(limit.min(snapshot.entries.len()));
    let mut truncated = false;

    for entry in &snapshot.entries {
        // Skip directories; preserve the original behavior that yields files
        // and symlinks but never directories.
        if matches!(entry.file_type, ScopeFileType::Dir) {
            continue;
        }

        // Check if the relative path matches any of the expanded patterns.
        // `rendered_path` is already the relative path as a UTF-8 string, so
        // matching the string directly is equivalent to `matches_path_with`
        // (which round-trips through `Path::to_str`).
        let matches = patterns.iter().zip(&pattern_suffixes).any(|(p, suffix)| {
            if let Some(suffix) = suffix
                && !entry.rendered_path.ends_with(suffix)
            {
                return false;
            }
            p.matches_with(&entry.rendered_path, match_options)
        });
        if !matches {
            continue;
        }

        files.push(entry.path.display().to_string());

        if files.len() >= limit {
            truncated = true;
            break;
        }
    }

    let text_output = if files.is_empty() {
        format!("No files match pattern: {}", req.pattern)
    } else {
        files.join("\n")
    };

    let mut payload = json!({
        "content": [{"type": "text", "text": text_output}],
        "isError": false,
        "pattern": req.pattern,
        "base_path": base_path,
        "count": files.len(),
        "files": files
    });

    if truncated && let Some(obj) = payload.as_object_mut() {
        obj.insert("truncated".to_string(), Value::Bool(true));
    }

    ToolCallOutcome::ok(payload)
}

define_mcp_tool! {
    GlobTool,
    name: "Glob",
    description: "Find files matching a glob pattern",
    schema: {
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "description": "Glob pattern with brace expansion (e.g., '**/*.rs', 'src/*.{ts,tsx}', '**/*.{cpp,h}')"
            },
            "path": {
                "type": "string",
                "description": "Base directory to search from"
            },
            "hidden": {
                "type": "boolean",
                "description": "Include hidden files"
            },
            "limit": {
                "type": "integer",
                "description": "Maximum number of matches to return"
            }
        },
        "required": ["pattern"],
        "additionalProperties": false
    },
    handler: handle_glob
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_EXPANDED_PATTERNS, RepoScopeKey, expand_braces, glob_traversal_max_depth, handle_glob,
        repo_scope_cache,
    };
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static GLOB_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct GlobTestDir {
        path: PathBuf,
    }

    impl GlobTestDir {
        fn new(name: &str) -> Self {
            let base = std::env::current_dir()
                .expect("current directory")
                .join("target")
                .join("glob-cache-tests");
            fs::create_dir_all(&base).expect("create test base directory");
            let unique = GLOB_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{name}-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).expect("create test directory");
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for GlobTestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn expands_common_brace_patterns() {
        let expanded = expand_braces("src/*.{ts,tsx}").expect("valid expansion");
        assert_eq!(expanded, vec!["src/*.ts", "src/*.tsx"]);
    }

    #[test]
    fn preserves_single_item_braces_as_literals() {
        let expanded = expand_braces("src/{literal}.rs").expect("valid expansion");
        assert_eq!(expanded, vec!["src/{literal}.rs"]);
    }

    #[test]
    fn expands_outer_group_when_inner_single_item_is_literal() {
        let expanded = expand_braces("{src,{tests}}/*.rs").expect("valid expansion");
        assert_eq!(expanded, vec!["src/*.rs", "{tests}/*.rs"]);
    }

    #[test]
    fn rejects_excessive_expansion_growth() {
        let mut pattern = String::new();
        for _ in 0..12 {
            pattern.push_str("{a,b}");
        }

        let err = expand_braces(&pattern).expect_err("expected expansion limit failure");
        assert!(err.contains(&MAX_EXPANDED_PATTERNS.to_string()));
    }

    #[test]
    fn required_literal_suffix_extracts_conservative_tails() {
        use super::required_literal_suffix;

        assert_eq!(required_literal_suffix("**/*.rs"), Some(".rs"));
        assert_eq!(required_literal_suffix("src/*.ts"), Some(".ts"));
        assert_eq!(required_literal_suffix("README.md"), Some("README.md"));
        // Tail inside or ending a character class must not become a prefilter.
        assert_eq!(required_literal_suffix("*.r[sx]"), None);
        // Separators in the tail are excluded: glob treats `/` and `\` as equal.
        assert_eq!(required_literal_suffix("docs/README.md"), None);
        assert_eq!(required_literal_suffix("src/**"), None);
        assert_eq!(required_literal_suffix("*?"), None);
    }

    #[test]
    fn required_literal_suffix_never_rejects_a_matching_path() {
        use super::required_literal_suffix;
        use glob::{MatchOptions, Pattern};

        let options = MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: true,
        };
        let patterns = [
            "**/*.rs",
            "*.r[sx]",
            "src/*.ts",
            "README.md",
            "docs/*.md",
            "a?c.txt",
            "*",
            "file.[tx]xt",
        ];
        let paths = [
            "lib.rs",
            "a.rx",
            "a.rs",
            "src\\x.ts",
            "src/x.ts",
            "README.md",
            "docs\\guide.md",
            "docs/guide.md",
            "abc.txt",
            "x",
            "nested\\deep\\mod.rs",
            "nested/deep/mod.rs",
            "file.txt",
            "file.xxt",
        ];

        for pattern in patterns {
            let compiled = Pattern::new(pattern).expect("valid pattern");
            let suffix = required_literal_suffix(pattern);
            for path in paths {
                if compiled.matches_with(path, options) {
                    assert!(
                        suffix.is_none_or(|suffix| path.ends_with(suffix)),
                        "prefilter for {pattern:?} must accept matching path {path:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn derives_bounded_traversal_depth_for_finite_patterns() {
        assert_eq!(
            glob_traversal_max_depth(&["*.rs".to_string(), "src/*.rs".to_string()]),
            Some(2)
        );
        assert_eq!(
            glob_traversal_max_depth(&["src/**/mod.rs".to_string()]),
            None
        );
    }

    #[test]
    fn scope_cache_returns_same_snapshot_for_repeat_glob_key() {
        let dir = GlobTestDir::new("repeat-snapshot");
        fs::write(dir.path().join("alpha.rs"), "fn alpha() {}").expect("write alpha");
        fs::write(dir.path().join("beta.txt"), "beta").expect("write beta");

        let key = RepoScopeKey {
            root: dir.path().to_path_buf(),
            hidden: false,
            follow: false,
            no_ignore: false,
            max_depth: None,
        };
        let deadline = Instant::now() + Duration::from_secs(5);

        let first = repo_scope_cache()
            .get_or_build(&key, deadline)
            .expect("initial snapshot");
        let second = repo_scope_cache()
            .get_or_build(&key, deadline)
            .expect("cached snapshot");

        assert!(
            Arc::ptr_eq(&first, &second),
            "repeated Glob scope lookups must reuse the cached snapshot"
        );
    }

    #[tokio::test]
    async fn handle_glob_filters_files_in_tempdir() {
        let dir = GlobTestDir::new("filter-correctness");
        fs::write(dir.path().join("alpha.rs"), "fn alpha() {}").expect("write alpha");
        fs::write(dir.path().join("beta.rs"), "fn beta() {}").expect("write beta");
        fs::write(dir.path().join("gamma.txt"), "gamma").expect("write gamma");

        let args = json!({
            "pattern": "*.rs",
            "path": dir.path().display().to_string(),
        });
        let resp = handle_glob(Some(json!(1)), args).await.0;
        assert_eq!(resp["isError"], false, "expected success: {resp}");

        let files = resp["files"]
            .as_array()
            .expect("files array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            files.len(),
            2,
            "expected exactly two .rs matches: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.ends_with("alpha.rs")),
            "expected alpha.rs in {files:?}"
        );
        assert!(
            files.iter().any(|f| f.ends_with("beta.rs")),
            "expected beta.rs in {files:?}"
        );
        assert!(
            files.iter().all(|f| !f.ends_with("gamma.txt")),
            "did not expect gamma.txt in {files:?}"
        );
    }

    #[tokio::test]
    async fn handle_glob_returns_sorted_limited_files() {
        let dir = GlobTestDir::new("sorted-limit");
        fs::write(dir.path().join("zeta.rs"), "fn zeta() {}").expect("write zeta");
        fs::write(dir.path().join("alpha.rs"), "fn alpha() {}").expect("write alpha");
        fs::write(dir.path().join("beta.rs"), "fn beta() {}").expect("write beta");

        let resp = handle_glob(
            Some(json!(1)),
            json!({
                "pattern": "*.rs",
                "path": dir.path().display().to_string(),
                "limit": 2,
            }),
        )
        .await
        .0;
        assert_eq!(resp["isError"], false, "expected success: {resp}");
        assert_eq!(resp["truncated"], true, "expected limit truncation: {resp}");

        let files = resp["files"]
            .as_array()
            .expect("files array")
            .iter()
            .map(|v| {
                PathBuf::from(v.as_str().unwrap_or_default())
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(files, vec!["alpha.rs", "beta.rs"]);
    }
}
