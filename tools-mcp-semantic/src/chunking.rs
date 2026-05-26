use crate::discovery::FileCandidate;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::OnceLock;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};

const MAX_SYMBOL_CHUNK_BYTES: usize = 32 * 1024;
const FALLBACK_CHUNK_LINES: usize = 100;
const FALLBACK_OVERLAP_LINES: usize = 15;

type CachedParser = Result<Parser, ()>;
type CachedTagsQuery = Result<Query, ()>;

static RUST_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static TYPESCRIPT_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static TSX_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static JAVASCRIPT_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static PYTHON_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static GO_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();

thread_local! {
    static RUST_PARSER: RefCell<CachedParser> =
        RefCell::new(parser_for_language(tree_sitter_rust::LANGUAGE.into()));
    static TYPESCRIPT_PARSER: RefCell<CachedParser> =
        RefCell::new(parser_for_language(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()));
    static TSX_PARSER: RefCell<CachedParser> =
        RefCell::new(parser_for_language(tree_sitter_typescript::LANGUAGE_TSX.into()));
    static JAVASCRIPT_PARSER: RefCell<CachedParser> =
        RefCell::new(parser_for_language(tree_sitter_javascript::LANGUAGE.into()));
    static PYTHON_PARSER: RefCell<CachedParser> =
        RefCell::new(parser_for_language(tree_sitter_python::LANGUAGE.into()));
    static GO_PARSER: RefCell<CachedParser> =
        RefCell::new(parser_for_language(tree_sitter_go::LANGUAGE.into()));
}

#[derive(Clone, Copy)]
enum TagsLanguage {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
}

impl TagsLanguage {
    fn language(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }

    fn tags_query(self) -> &'static str {
        match self {
            Self::Rust => tree_sitter_rust::TAGS_QUERY,
            Self::TypeScript | Self::Tsx => tree_sitter_typescript::TAGS_QUERY,
            Self::JavaScript => tree_sitter_javascript::TAGS_QUERY,
            Self::Python => tree_sitter_python::TAGS_QUERY,
            Self::Go => tree_sitter_go::TAGS_QUERY,
        }
    }

    fn query_cache(self) -> &'static OnceLock<CachedTagsQuery> {
        match self {
            Self::Rust => &RUST_TAGS_QUERY,
            Self::TypeScript => &TYPESCRIPT_TAGS_QUERY,
            Self::Tsx => &TSX_TAGS_QUERY,
            Self::JavaScript => &JAVASCRIPT_TAGS_QUERY,
            Self::Python => &PYTHON_TAGS_QUERY,
            Self::Go => &GO_TAGS_QUERY,
        }
    }

    fn parse(self, source: &str) -> Option<Tree> {
        match self {
            Self::Rust => parse_with_cached_parser(&RUST_PARSER, source),
            Self::TypeScript => parse_with_cached_parser(&TYPESCRIPT_PARSER, source),
            Self::Tsx => parse_with_cached_parser(&TSX_PARSER, source),
            Self::JavaScript => parse_with_cached_parser(&JAVASCRIPT_PARSER, source),
            Self::Python => parse_with_cached_parser(&PYTHON_PARSER, source),
            Self::Go => parse_with_cached_parser(&GO_PARSER, source),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CodeChunk {
    pub(crate) chunk_id: String,
    pub(crate) path: String,
    pub(crate) language: String,
    pub(crate) symbol: Option<String>,
    pub(crate) start_line: u64,
    pub(crate) end_line: u64,
    pub(crate) content: String,
    pub(crate) content_hash: String,
    pub(crate) file_hash: String,
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub(crate) fn chunk_source(file: &FileCandidate, source: &str, file_hash: &str) -> Vec<CodeChunk> {
    let mut chunks = match file.language.as_str() {
        "rust" => tags_query_chunks(file, source, file_hash, TagsLanguage::Rust),
        "typescript" => tags_query_chunks(file, source, file_hash, TagsLanguage::TypeScript),
        "tsx" => tags_query_chunks(file, source, file_hash, TagsLanguage::Tsx),
        "javascript" => tags_query_chunks(file, source, file_hash, TagsLanguage::JavaScript),
        "python" => tags_query_chunks(file, source, file_hash, TagsLanguage::Python),
        "go" => tags_query_chunks(file, source, file_hash, TagsLanguage::Go),
        "markdown" => markdown_chunks(file, source, file_hash),
        _ => Vec::new(),
    };

    if chunks.is_empty() {
        chunks = fallback_line_chunks(file, source, file_hash, None);
    }
    chunks
}

fn tags_query_chunks(
    file: &FileCandidate,
    source: &str,
    file_hash: &str,
    language: TagsLanguage,
) -> Vec<CodeChunk> {
    let Some(tree) = language.parse(source) else {
        return Vec::new();
    };
    let Some(query) = cached_tags_query(language) else {
        return Vec::new();
    };

    let source_bytes = source.as_bytes();
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source_bytes);
    let mut seen_ranges = HashSet::new();
    let mut chunks = Vec::new();

    while let Some(query_match) = matches.next() {
        let mut definition_node = None;
        let mut symbol = None;

        for capture in query_match.captures {
            let capture_name = capture_names[capture.index as usize];
            if capture_name == "name" {
                let text = capture
                    .node
                    .utf8_text(source_bytes)
                    .unwrap_or_default()
                    .trim();
                if !text.is_empty() {
                    symbol = Some(text.to_string());
                }
            } else if capture_name.starts_with("definition.") && definition_node.is_none() {
                definition_node = Some(capture.node);
            }
        }

        let Some(node) = definition_node else {
            continue;
        };
        let range = node.byte_range();
        let start_byte = range.start;
        let end_byte = range.end;
        if range.is_empty()
            || end_byte > source_bytes.len()
            || !seen_ranges.insert((start_byte, end_byte))
        {
            continue;
        }

        let content = node.utf8_text(source_bytes).unwrap_or_default().trim();
        if content.is_empty() {
            continue;
        }

        if content.len() > MAX_SYMBOL_CHUNK_BYTES {
            chunks.extend(fallback_line_chunks(file, content, file_hash, symbol));
            continue;
        }

        let start_line = node.start_position().row as u64 + 1;
        let end_line = node.end_position().row as u64 + 1;
        let content_hash = hash_bytes(content.as_bytes());
        chunks.push(build_chunk(
            file,
            symbol,
            start_line,
            end_line,
            content.to_string(),
            content_hash,
            file_hash,
        ));
    }

    chunks.sort_by(|left, right| {
        (left.start_line, left.end_line, left.chunk_id.as_str()).cmp(&(
            right.start_line,
            right.end_line,
            right.chunk_id.as_str(),
        ))
    });
    chunks
}

fn parser_for_language(language: Language) -> CachedParser {
    let mut parser = Parser::new();
    parser.set_language(&language).map_err(|_| ())?;
    Ok(parser)
}

fn parse_with_cached_parser(
    parser_cache: &'static std::thread::LocalKey<RefCell<CachedParser>>,
    source: &str,
) -> Option<Tree> {
    parser_cache.with(|parser| {
        let mut parser = parser.borrow_mut();
        let parser = parser.as_mut().ok()?;
        parser.reset();
        parser.parse(source, None)
    })
}

fn cached_tags_query(language: TagsLanguage) -> Option<&'static Query> {
    let query = language
        .query_cache()
        .get_or_init(|| Query::new(&language.language(), language.tags_query()).map_err(|_| ()));
    query.as_ref().ok()
}

fn markdown_chunks(file: &FileCandidate, source: &str, file_hash: &str) -> Vec<CodeChunk> {
    let lines = source.lines().collect::<Vec<_>>();
    let mut headings = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let depth = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&depth)
            && trimmed
                .get(depth..)
                .is_some_and(|rest| rest.starts_with(' '))
        {
            headings.push(index);
        }
    }

    if headings.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::with_capacity(headings.len());
    for (heading_index, start) in headings.iter().copied().enumerate() {
        let end_exclusive = headings
            .get(heading_index + 1)
            .copied()
            .unwrap_or(lines.len());
        let Some((content, content_hash)) =
            join_lines_trimmed_and_hash(&lines[start..end_exclusive])
        else {
            continue;
        };
        let symbol = lines[start]
            .trim_start_matches('#')
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        chunks.push(build_chunk(
            file,
            (!symbol.is_empty()).then_some(symbol),
            start as u64 + 1,
            end_exclusive as u64,
            content,
            content_hash,
            file_hash,
        ));
    }
    chunks
}

fn fallback_line_chunks(
    file: &FileCandidate,
    source: &str,
    file_hash: &str,
    symbol: Option<String>,
) -> Vec<CodeChunk> {
    let lines = source.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Vec::new();
    }

    let stride = FALLBACK_CHUNK_LINES
        .saturating_sub(FALLBACK_OVERLAP_LINES)
        .max(1);
    let chunk_count = if lines.len() <= FALLBACK_CHUNK_LINES {
        1
    } else {
        1 + (lines.len() - FALLBACK_CHUNK_LINES).div_ceil(stride)
    };
    let mut chunks = Vec::with_capacity(chunk_count);
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + FALLBACK_CHUNK_LINES).min(lines.len());
        if let Some((content, content_hash)) = join_lines_trimmed_and_hash(&lines[start..end]) {
            chunks.push(build_chunk(
                file,
                symbol.clone(),
                start as u64 + 1,
                end as u64,
                content,
                content_hash,
                file_hash,
            ));
        }

        if end == lines.len() {
            break;
        }
        start = end.saturating_sub(FALLBACK_OVERLAP_LINES);
    }
    chunks
}

fn join_lines_trimmed_and_hash(lines: &[&str]) -> Option<(String, String)> {
    let mut first_content_line = None;
    let mut last_content_line = None;
    for (index, line) in lines.iter().enumerate() {
        if !line.trim().is_empty() {
            first_content_line.get_or_insert(index);
            last_content_line = Some(index);
        }
    }
    let first_content_line = first_content_line?;
    let last_content_line = last_content_line.unwrap_or(first_content_line);
    let selected_lines = &lines[first_content_line..=last_content_line];
    let last_selected_line = selected_lines.len().saturating_sub(1);

    let capacity = selected_lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let trimmed = if index == 0 && index == last_selected_line {
                line.trim()
            } else if index == 0 {
                line.trim_start()
            } else if index == last_selected_line {
                line.trim_end()
            } else {
                line
            };
            trimmed.len()
        })
        .sum::<usize>()
        + selected_lines.len().saturating_sub(1);
    let mut content = String::with_capacity(capacity);
    let mut hasher = Sha256::new();
    for (index, line) in selected_lines.iter().enumerate() {
        if index > 0 {
            content.push('\n');
            hasher.update(b"\n");
        }
        let trimmed = if index == 0 && index == last_selected_line {
            line.trim()
        } else if index == 0 {
            line.trim_start()
        } else if index == last_selected_line {
            line.trim_end()
        } else {
            line
        };
        content.push_str(trimmed);
        hasher.update(trimmed.as_bytes());
    }
    Some((content, hex::encode(hasher.finalize())))
}

fn build_chunk(
    file: &FileCandidate,
    symbol: Option<String>,
    start_line: u64,
    end_line: u64,
    content: String,
    content_hash: String,
    file_hash: &str,
) -> CodeChunk {
    let mut id_hasher = Sha256::new();
    id_hasher.update(file.relative_path.as_bytes());
    id_hasher.update(start_line.to_le_bytes());
    id_hasher.update(end_line.to_le_bytes());
    id_hasher.update(content_hash.as_bytes());
    let chunk_id = hex::encode(id_hasher.finalize());

    CodeChunk {
        chunk_id,
        path: file.relative_path.clone(),
        language: file.language.clone(),
        symbol,
        start_line,
        end_line,
        content,
        content_hash,
        file_hash: file_hash.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{chunk_source, hash_bytes};
    use crate::discovery::FileCandidate;
    use std::path::PathBuf;

    fn file(path: &str, language: &str) -> FileCandidate {
        FileCandidate {
            absolute_path: PathBuf::from(path),
            relative_path: path.to_string(),
            language: language.to_string(),
        }
    }

    #[test]
    fn chunks_rust_functions_with_line_spans() {
        let source = "fn alpha() {}\n\nfn beta() {\n    alpha();\n}\n";
        let file_hash = hash_bytes(source.as_bytes());
        let chunks = chunk_source(&file("src/lib.rs", "rust"), source, &file_hash);

        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.symbol.as_deref() == Some("alpha"))
        );
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.symbol.as_deref() == Some("beta"))
        );
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.start_line <= chunk.end_line)
        );

        let beta = chunks
            .iter()
            .find(|chunk| chunk.symbol.as_deref() == Some("beta"))
            .expect("beta chunk");
        assert_eq!(beta.path, "src/lib.rs");
        assert_eq!(beta.language, "rust");
        assert_eq!(beta.start_line, 3);
        assert_eq!(beta.end_line, 5);
        assert_eq!(beta.content, "fn beta() {\n    alpha();\n}");
        assert_eq!(beta.content_hash, hash_bytes(beta.content.as_bytes()));
        assert_eq!(beta.file_hash, file_hash);
    }

    #[test]
    fn chunks_markdown_by_heading() {
        let source = "# One\nbody\n## Two\nmore\n";
        let chunks = chunk_source(
            &file("README.md", "markdown"),
            source,
            &hash_bytes(source.as_bytes()),
        );

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].symbol.as_deref(), Some("One"));
        assert_eq!(chunks[1].symbol.as_deref(), Some("Two"));
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 2);
        assert_eq!(chunks[0].content, "# One\nbody");
        assert_eq!(chunks[1].start_line, 3);
        assert_eq!(chunks[1].end_line, 4);
        assert_eq!(chunks[1].content, "## Two\nmore");
    }

    #[test]
    fn chunks_markdown_trims_outer_blank_lines_and_keeps_internal_spacing() {
        let source = "# One\n\nbody 1\n\nbody 2\n\n## Two\nline 2\n";
        let chunks = chunk_source(
            &file("README.md", "markdown"),
            source,
            &hash_bytes(source.as_bytes()),
        );

        assert_eq!(chunks.len(), 2);

        let one = chunks
            .iter()
            .find(|chunk| chunk.symbol.as_deref() == Some("One"))
            .expect("one chunk");
        assert_eq!(one.start_line, 1);
        assert_eq!(one.end_line, 6);
        assert_eq!(one.content, "# One\n\nbody 1\n\nbody 2");
        assert_eq!(one.content_hash, hash_bytes(one.content.as_bytes()));

        let two = chunks
            .iter()
            .find(|chunk| chunk.symbol.as_deref() == Some("Two"))
            .expect("two chunk");
        assert_eq!(two.start_line, 7);
        assert_eq!(two.end_line, 8);
        assert_eq!(two.content, "## Two\nline 2");
        assert_eq!(two.content_hash, hash_bytes(two.content.as_bytes()));
    }

    #[test]
    fn fallback_chunks_preserve_overlap_and_metadata() {
        let source = (1..=110)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let file_hash = hash_bytes(source.as_bytes());
        let chunks = chunk_source(&file("notes.txt", "text"), &source, &file_hash);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].path, "notes.txt");
        assert_eq!(chunks[0].language, "text");
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 100);
        assert!(chunks[0].content.starts_with("line 1\nline 2"));
        assert!(chunks[0].content.ends_with("line 100"));
        assert_eq!(
            chunks[0].content_hash,
            hash_bytes(chunks[0].content.as_bytes())
        );
        assert_eq!(chunks[0].file_hash, file_hash);

        assert_eq!(chunks[1].start_line, 86);
        assert_eq!(chunks[1].end_line, 110);
        assert!(chunks[1].content.starts_with("line 86\nline 87"));
        assert!(chunks[1].content.ends_with("line 110"));
        assert_eq!(
            chunks[1].content_hash,
            hash_bytes(chunks[1].content.as_bytes())
        );
    }
}
