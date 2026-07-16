use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::define_mcp_tool;
use tools_mcp_core::validation;

const DEFAULT_EXCLUDED_DIRECTORIES: [&str; 3] = ["target", ".git", ".claude"];
const MAX_EXCLUDED_DIRECTORIES: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CountLinesRequest {
    extension: String,
    #[serde(default = "default_path")]
    path: String,
    #[serde(default = "default_excluded_directories")]
    exclude: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct DirectoryCount {
    directory: String,
    path: String,
    files: u64,
    lines: u64,
}

fn default_path() -> String {
    ".".to_string()
}

fn default_excluded_directories() -> Vec<String> {
    DEFAULT_EXCLUDED_DIRECTORIES
        .into_iter()
        .map(str::to_string)
        .collect()
}

async fn handle_count_lines(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<CountLinesRequest>(&args) {
        Ok(req) => req,
        Err(outcome) => return outcome,
    };

    if let Err(outcome) = validation::validate_non_empty(&req.extension, "extension", None) {
        return outcome;
    }
    if let Err(outcome) = validation::validate_non_empty(&req.path, "path", None) {
        return outcome;
    }

    let extension = match normalize_extension(&req.extension) {
        Ok(extension) => extension,
        Err(message) => return ToolCallOutcome::err(message),
    };
    let excluded_directories = match normalize_excluded_directories(req.exclude) {
        Ok(excluded) => excluded,
        Err(message) => return ToolCallOutcome::err(message),
    };

    let root = PathBuf::from(&req.path);
    let metadata = match fs::metadata(&root) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return ToolCallOutcome::err(format!(
                "path not found: {}. Remediation: set 'path' to an existing directory or omit it to use '.'.",
                root.display()
            ));
        }
        Err(err) => {
            return ToolCallOutcome::err(format!(
                "failed to inspect path {}: {err}. Remediation: check path permissions and retry.",
                root.display()
            ));
        }
    };
    if !metadata.is_dir() {
        return ToolCallOutcome::err(format!(
            "not a directory: {}. Remediation: pass a directory path to 'path'.",
            root.display()
        ));
    }

    let scan_root = root.clone();
    let scan_extension = extension.clone();
    let scan_exclusions = excluded_directories.clone();
    let counts = match tokio::task::spawn_blocking(move || {
        count_directories(&scan_root, &scan_extension, &scan_exclusions)
    })
    .await
    {
        Ok(Ok(counts)) => counts,
        Ok(Err(message)) => return ToolCallOutcome::err(message),
        Err(err) => {
            return ToolCallOutcome::err(format!(
                "count_lines worker failed: {err}. Remediation: retry the request."
            ));
        }
    };

    let (total_files, total_lines) = match counts.iter().try_fold(
        (0u64, 0u64),
        |(files, lines), count| {
            Some((
                files.checked_add(count.files)?,
                lines.checked_add(count.lines)?,
            ))
        },
    ) {
        Some(totals) => totals,
        None => {
            return ToolCallOutcome::err(
                "count_lines totals exceed the supported u64 range. Remediation: scan a narrower path.",
            );
        }
    };
    let text = render_table(&counts);

    ToolCallOutcome::ok_text_with(
        text,
        [
            ("path", json!(root.display().to_string())),
            ("extension", json!(extension)),
            ("excluded_directories", json!(excluded_directories)),
            ("directory_count", json!(counts.len())),
            ("total_files", json!(total_files)),
            ("total_lines", json!(total_lines)),
            ("directories", json!(counts)),
        ],
    )
}

fn normalize_extension(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    let extension = trimmed
        .strip_prefix("*.")
        .or_else(|| trimmed.strip_prefix('.'))
        .unwrap_or(trimmed);

    if extension.is_empty() {
        return Err(
            "extension must name a file extension such as 'rs', '.rs', or '*.rs'.".to_string(),
        );
    }
    if extension
        .chars()
        .any(|ch| matches!(ch, '/' | '\\' | '*' | '?' | '[' | ']'))
    {
        return Err(format!(
            "invalid extension '{raw}': extensions cannot contain path separators or glob metacharacters. Remediation: use a value such as 'rs', '.rs', or '*.rs'."
        ));
    }

    Ok(extension.to_string())
}

fn normalize_excluded_directories(excluded: Vec<String>) -> Result<Vec<String>, String> {
    if excluded.len() > MAX_EXCLUDED_DIRECTORIES {
        return Err(format!(
            "exclude accepts at most {MAX_EXCLUDED_DIRECTORIES} directory names"
        ));
    }

    let mut normalized = Vec::with_capacity(excluded.len());
    for value in excluded {
        let name = value.trim();
        if name.is_empty() {
            return Err("exclude entries must be non-empty directory names".to_string());
        }

        let mut components = Path::new(name).components();
        let is_single_name =
            matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
        if !is_single_name {
            return Err(format!(
                "invalid excluded directory '{value}': entries must be directory names, not paths"
            ));
        }

        if !normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(name))
        {
            normalized.push(name.to_string());
        }
    }

    Ok(normalized)
}

fn count_directories(
    root: &Path,
    extension: &str,
    excluded_directories: &[String],
) -> Result<Vec<DirectoryCount>, String> {
    let entries = fs::read_dir(root).map_err(|err| {
        format!(
            "failed to read directory {}: {err}. Remediation: check directory permissions and retry.",
            root.display()
        )
    })?;

    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| {
            format!(
                "failed to read an entry under {}: {err}. Remediation: check directory permissions and retry.",
                root.display()
            )
        })?;
        let file_type = entry.file_type().map_err(|err| {
            format!(
                "failed to inspect {}: {err}. Remediation: check path permissions and retry.",
                entry.path().display()
            )
        })?;
        if !file_type.is_dir()
            || is_excluded_directory_name(&entry.file_name(), excluded_directories)
        {
            continue;
        }

        directories.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.path(),
        ));
    }
    directories.sort_by(|left, right| left.0.cmp(&right.0));

    let mut counts = Vec::with_capacity(directories.len());
    for (directory, path) in directories {
        let (files, lines) = count_directory(&path, extension, excluded_directories)?;
        counts.push(DirectoryCount {
            directory,
            path: path.display().to_string(),
            files,
            lines,
        });
    }

    counts.sort_by(|left, right| {
        right
            .lines
            .cmp(&left.lines)
            .then_with(|| right.files.cmp(&left.files))
            .then_with(|| left.directory.cmp(&right.directory))
    });
    Ok(counts)
}

fn count_directory(
    path: &Path,
    extension: &str,
    excluded_directories: &[String],
) -> Result<(u64, u64), String> {
    let exclusions = Arc::new(excluded_directories.to_vec());
    let filter_exclusions = Arc::clone(&exclusions);
    let mut builder = WalkBuilder::new(path);
    builder
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            entry.depth() == 0
                || !entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_dir())
                || !is_excluded_directory_name(entry.file_name(), &filter_exclusions)
        });

    let mut files = 0u64;
    let mut lines = 0u64;
    for walked in builder.build() {
        let entry = walked.map_err(|err| {
            format!(
                "failed to walk directory {}: {err}. Remediation: check directory permissions or add an inaccessible directory name to 'exclude'.",
                path.display()
            )
        })?;
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
            || !matches_extension(entry.file_name(), extension)
        {
            continue;
        }

        let file_lines = count_file_lines(entry.path()).map_err(|err| {
            format!(
                "failed to count lines in {}: {err}. Remediation: check file permissions and retry.",
                entry.path().display()
            )
        })?;
        files = files
            .checked_add(1)
            .ok_or_else(|| format!("file count overflowed while scanning {}", path.display()))?;
        lines = lines.checked_add(file_lines).ok_or_else(|| {
            format!(
                "line count overflowed while scanning directory {}",
                path.display()
            )
        })?;
    }

    Ok((files, lines))
}

fn is_excluded_directory_name(name: &OsStr, excluded_directories: &[String]) -> bool {
    let name = name.to_string_lossy();
    excluded_directories
        .iter()
        .any(|excluded| name.eq_ignore_ascii_case(excluded))
}

fn matches_extension(name: &OsStr, extension: &str) -> bool {
    let name = name.to_string_lossy();
    let suffix = format!(".{extension}");
    let name_bytes = name.as_bytes();
    let suffix_bytes = suffix.as_bytes();

    name_bytes.len() >= suffix_bytes.len()
        && name_bytes[name_bytes.len() - suffix_bytes.len()..].eq_ignore_ascii_case(suffix_bytes)
}

fn count_file_lines(path: &Path) -> io::Result<u64> {
    let file = File::open(path)?;
    count_reader_lines(file)
}

fn count_reader_lines(mut reader: impl Read) -> io::Result<u64> {
    let mut buffer = [0u8; 64 * 1024];
    let mut lines = 0u64;
    let mut line_has_content = false;
    let mut previous_was_cr = false;

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        for &byte in &buffer[..read] {
            match byte {
                b'\r' => {
                    if line_has_content {
                        lines = checked_increment(lines)?;
                    }
                    line_has_content = false;
                    previous_was_cr = true;
                }
                b'\n' => {
                    if !previous_was_cr && line_has_content {
                        lines = checked_increment(lines)?;
                    }
                    line_has_content = false;
                    previous_was_cr = false;
                }
                _ => {
                    line_has_content = true;
                    previous_was_cr = false;
                }
            }
        }
    }

    if line_has_content {
        lines = checked_increment(lines)?;
    }
    Ok(lines)
}

fn checked_increment(value: u64) -> io::Result<u64> {
    value.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "line count exceeds the supported u64 range",
        )
    })
}

fn render_table(counts: &[DirectoryCount]) -> String {
    if counts.is_empty() {
        return String::new();
    }

    let directory_width = counts
        .iter()
        .map(|count| count.directory.chars().count())
        .max()
        .unwrap_or(0)
        .max("Directory".len());
    let files_width = counts
        .iter()
        .map(|count| count.files.to_string().len())
        .max()
        .unwrap_or(0)
        .max("Files".len());
    let lines_width = counts
        .iter()
        .map(|count| count.lines.to_string().len())
        .max()
        .unwrap_or(0)
        .max("Lines".len());

    let mut rows = Vec::with_capacity(counts.len() + 2);
    rows.push(format!(
        "{:<directory_width$}  {:>files_width$}  {:>lines_width$}",
        "Directory", "Files", "Lines"
    ));
    rows.push(format!(
        "{:-<directory_width$}  {:-<files_width$}  {:-<lines_width$}",
        "", "", ""
    ));
    rows.extend(counts.iter().map(|count| {
        format!(
            "{:<directory_width$}  {:>files_width$}  {:>lines_width$}",
            count.directory, count.files, count.lines
        )
    }));
    rows.join("\n")
}

define_mcp_tool! {
    CountLinesTool,
    name: "CountLines",
    description: "Count files with a given extension and their non-empty lines under each immediate child directory.",
    schema: {
        "type": "object",
        "properties": {
            "extension": {
                "type": "string",
                "minLength": 1,
                "description": "File extension to count, such as 'rs', '.rs', or '*.rs'"
            },
            "path": {
                "type": "string",
                "minLength": 1,
                "default": ".",
                "description": "Root whose immediate child directories are summarized"
            },
            "exclude": {
                "type": "array",
                "maxItems": 128,
                "uniqueItems": true,
                "default": ["target", ".git", ".claude"],
                "items": {
                    "type": "string",
                    "minLength": 1
                },
                "description": "Directory names to exclude at the root and at every recursive depth; pass [] to disable exclusions"
            }
        },
        "required": ["extension"],
        "additionalProperties": false
    },
    handler: handle_count_lines
}

#[cfg(test)]
mod tests {
    use super::{
        count_reader_lines, handle_count_lines, normalize_excluded_directories, normalize_extension,
    };
    use serde_json::json;
    use std::io::Cursor;
    use tempfile::tempdir;

    #[test]
    fn normalizes_supported_extension_forms() {
        assert_eq!(normalize_extension("rs").expect("plain extension"), "rs");
        assert_eq!(normalize_extension(".rs").expect("dotted extension"), "rs");
        assert_eq!(
            normalize_extension("*.rs").expect("glob-like extension"),
            "rs"
        );
        assert_eq!(
            normalize_extension("tar.gz").expect("compound extension"),
            "tar.gz"
        );

        assert!(normalize_extension(".").is_err());
        assert!(normalize_extension("src/rs").is_err());
        assert!(normalize_extension("r*s").is_err());
    }

    #[test]
    fn validates_and_deduplicates_excluded_directory_names() {
        assert_eq!(
            normalize_excluded_directories(vec![
                "target".to_string(),
                "TARGET".to_string(),
                " vendor ".to_string(),
            ])
            .expect("valid exclusions"),
            vec!["target", "vendor"]
        );
        assert!(normalize_excluded_directories(vec!["".to_string()]).is_err());
        assert!(normalize_excluded_directories(vec!["nested/path".to_string()]).is_err());
        assert!(normalize_excluded_directories(vec!["..".to_string()]).is_err());
    }

    #[test]
    fn counts_non_empty_lf_crlf_cr_and_unterminated_lines() {
        for (contents, expected) in [
            (b"".as_slice(), 0),
            (b"one".as_slice(), 1),
            (b"one\n".as_slice(), 1),
            (b"one\n\ntwo".as_slice(), 2),
            (b"one\n   \ntwo".as_slice(), 3),
            (b"one\r\ntwo\r\n".as_slice(), 2),
            (b"one\rtwo\r".as_slice(), 2),
            (b"\xff\n\xfe".as_slice(), 2),
        ] {
            assert_eq!(
                count_reader_lines(Cursor::new(contents)).expect("line count"),
                expected,
                "contents: {contents:?}"
            );
        }
    }

    #[tokio::test]
    async fn counts_matching_files_by_directory_and_applies_default_exclusions() {
        let root = tempdir().expect("tempdir");
        let alpha = root.path().join("alpha");
        let beta = root.path().join("beta");
        let gamma = root.path().join("gamma");
        std::fs::create_dir_all(alpha.join("src")).expect("alpha src");
        std::fs::create_dir_all(alpha.join("target")).expect("alpha target");
        std::fs::create_dir_all(beta.join(".git")).expect("beta git");
        std::fs::create_dir_all(&gamma).expect("gamma");
        std::fs::create_dir_all(root.path().join("target")).expect("root target");
        std::fs::create_dir_all(root.path().join(".claude")).expect("root claude");

        std::fs::write(alpha.join("src").join("lib.rs"), "one\ntwo\n").expect("alpha lib");
        std::fs::write(alpha.join("UPPER.RS"), "three").expect("alpha uppercase");
        std::fs::write(
            alpha.join("target").join("generated.rs"),
            "ignored\nignored\n",
        )
        .expect("alpha generated");
        std::fs::write(alpha.join("notes.txt"), "ignored\n").expect("alpha text");
        std::fs::write(beta.join("main.rs"), "one\r\ntwo\r\nthree\r\nfour").expect("beta main");
        std::fs::write(beta.join(".git").join("hidden.rs"), "ignored\n").expect("beta hidden");
        std::fs::write(root.path().join("target").join("root.rs"), "ignored\n")
            .expect("root target file");

        let response = handle_count_lines(
            Some(json!(1)),
            json!({
                "path": root.path().display().to_string(),
                "extension": "*.rs"
            }),
        )
        .await
        .0;

        assert_eq!(response["isError"], false, "expected success: {response}");
        assert_eq!(response["extension"], "rs");
        assert_eq!(
            response["excluded_directories"],
            json!(["target", ".git", ".claude"])
        );
        assert_eq!(response["directory_count"], 3);
        assert_eq!(response["total_files"], 3);
        assert_eq!(response["total_lines"], 7);
        assert_eq!(
            response["directories"],
            json!([
                {
                    "directory": "beta",
                    "path": beta.display().to_string(),
                    "files": 1,
                    "lines": 4
                },
                {
                    "directory": "alpha",
                    "path": alpha.display().to_string(),
                    "files": 2,
                    "lines": 3
                },
                {
                    "directory": "gamma",
                    "path": gamma.display().to_string(),
                    "files": 0,
                    "lines": 0
                }
            ])
        );
        assert_eq!(
            response["content"][0]["text"],
            "Directory  Files  Lines\n---------  -----  -----\nbeta           1      4\nalpha          2      3\ngamma          0      0"
        );
    }

    #[tokio::test]
    async fn empty_exclude_list_counts_target_directories() {
        let root = tempdir().expect("tempdir");
        let crate_dir = root.path().join("crate");
        let nested_target = crate_dir.join("target");
        let root_target = root.path().join("target");
        std::fs::create_dir_all(&nested_target).expect("nested target");
        std::fs::create_dir_all(&root_target).expect("root target");
        std::fs::write(nested_target.join("nested.rs"), "one\n").expect("nested file");
        std::fs::write(root_target.join("root.rs"), "one\ntwo\n").expect("root file");

        let response = handle_count_lines(
            Some(json!(1)),
            json!({
                "path": root.path().display().to_string(),
                "extension": "rs",
                "exclude": []
            }),
        )
        .await
        .0;

        assert_eq!(response["isError"], false, "expected success: {response}");
        assert_eq!(response["directory_count"], 2);
        assert_eq!(response["total_files"], 2);
        assert_eq!(response["total_lines"], 3);
        assert_eq!(response["directories"][0]["directory"], "target");
        assert_eq!(response["directories"][0]["lines"], 2);
        assert_eq!(response["directories"][1]["directory"], "crate");
        assert_eq!(response["directories"][1]["lines"], 1);
    }

    #[tokio::test]
    async fn rejects_non_directory_paths() {
        let root = tempdir().expect("tempdir");
        let file = root.path().join("file.txt");
        std::fs::write(&file, "content").expect("file");

        let response = handle_count_lines(
            Some(json!(1)),
            json!({
                "path": file.display().to_string(),
                "extension": "rs"
            }),
        )
        .await
        .0;

        assert_eq!(response["isError"], true);
        assert!(
            response["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("not a directory"))
        );
    }
}
