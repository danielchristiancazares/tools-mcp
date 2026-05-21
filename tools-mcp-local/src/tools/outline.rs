use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as FmtWrite;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tools_mcp_core::ToolCallOutcome;
use tools_mcp_core::define_mcp_tool;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator, Tree};

use crate::tools::scope_cache::{OutlineKey, outline_ast_cache};

type CachedTagsQuery = Result<Arc<Query>, String>;

static RUST_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static TYPESCRIPT_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static TSX_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static JAVASCRIPT_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static PYTHON_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();
static GO_TAGS_QUERY: OnceLock<CachedTagsQuery> = OnceLock::new();

const SUPPORTED_OUTLINE_EXTENSIONS: &[&str] = &[
    ".cpp",
    ".cxx",
    ".cc",
    ".h",
    ".hpp",
    ".hxx",
    ".rs",
    ".ts",
    ".tsx",
    ".js",
    ".mjs",
    ".cjs",
    ".jsx",
    ".py",
    ".pyi",
    ".go",
    ".md",
    ".markdown",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutlineLanguage {
    Cpp,
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    Markdown,
    Unsupported,
}

impl OutlineLanguage {
    fn cache_language(self, include_private: bool) -> Option<String> {
        let base = match self {
            OutlineLanguage::Cpp => "cpp",
            OutlineLanguage::Rust => "rust",
            OutlineLanguage::TypeScript => "typescript",
            OutlineLanguage::Tsx => "tsx",
            OutlineLanguage::JavaScript => "javascript",
            OutlineLanguage::Python => "python",
            OutlineLanguage::Go => "go",
            OutlineLanguage::Markdown => "markdown",
            OutlineLanguage::Unsupported => return None,
        };
        // include_private only affects C++ rendering; encode it in the cache
        // language so toggling the flag does not serve a stale outline.
        if include_private && matches!(self, OutlineLanguage::Cpp) {
            Some(format!("{base}+private"))
        } else {
            Some(base.to_string())
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutlineRequest {
    path: String,
    #[serde(default)]
    include_private: Option<bool>,
}

async fn handle_outline(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<OutlineRequest>(&args) {
        Ok(r) => r,
        Err(o) => return o,
    };

    outline_for_path(&req.path, req.include_private.unwrap_or(false)).await
}

async fn outline_for_path(path_str: &str, include_private: bool) -> ToolCallOutcome {
    let path = Path::new(path_str);
    if !path.exists() {
        return ToolCallOutcome::err(format!("file not found: {}", path.display()));
    }

    let extension = normalized_extension(path);
    let language = language_for_extension(extension.as_deref());

    let source = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) => {
            return ToolCallOutcome::err(format!("failed to read file: {e}"));
        }
    };

    // Build a cache key only for supported languages whose metadata we can read.
    // Unsupported extensions and metadata failures both bypass the cache and let
    // the existing code paths surface their original error messages verbatim.
    let cache_key = match language.cache_language(include_private) {
        Some(lang) => match std::fs::metadata(path) {
            Ok(meta) => Some(OutlineKey {
                path: path.to_path_buf(),
                language: lang,
                modified: meta.modified().ok(),
                len: meta.len(),
                content_hash: outline_content_hash(source.as_bytes()),
            }),
            Err(_) => None,
        },
        None => None,
    };

    if let Some(ref key) = cache_key
        && let Some(rendered) = outline_ast_cache().get(key)
    {
        let payload = json!({
            "content": [{"type": "text", "text": rendered.as_str()}],
            "isError": false,
            "path": path_str,
            "bytes": key.len as usize,
            "outline_bytes": rendered.len(),
        });
        return ToolCallOutcome::ok(payload);
    }

    let output = match render_outline(path, &source, include_private) {
        Ok(output) => output,
        Err(outcome) => return outcome,
    };

    let arc = Arc::new(output);
    if let Some(key) = cache_key {
        outline_ast_cache().insert(key, arc.clone());
    }

    let payload = json!({
        "content": [{"type": "text", "text": arc.as_str()}],
        "isError": false,
        "path": path_str,
        "bytes": source.len(),
        "outline_bytes": arc.len(),
    });

    ToolCallOutcome::ok(payload)
}

fn outline_content_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn render_outline(
    path: &Path,
    source: &str,
    include_private: bool,
) -> Result<String, ToolCallOutcome> {
    let extension = normalized_extension(path);
    let language = language_for_extension(extension.as_deref());

    match language {
        OutlineLanguage::Cpp => extract_cpp_outline(source, include_private),
        OutlineLanguage::Rust
        | OutlineLanguage::TypeScript
        | OutlineLanguage::Tsx
        | OutlineLanguage::JavaScript
        | OutlineLanguage::Python
        | OutlineLanguage::Go => extract_outline_with_tags_query(source, language),
        OutlineLanguage::Markdown => Ok(extract_markdown_outline(source)),
        OutlineLanguage::Unsupported => Err(ToolCallOutcome::err_with(
            "unsupported language for outline",
            [
                ("path", json!(path.display().to_string())),
                ("extension", json!(extension)),
                ("supported", json!(SUPPORTED_OUTLINE_EXTENSIONS)),
            ],
        )),
    }
}

fn normalized_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn language_for_extension(extension: Option<&str>) -> OutlineLanguage {
    match extension {
        Some("cpp" | "cxx" | "cc" | "h" | "hpp" | "hxx") => OutlineLanguage::Cpp,
        Some("rs") => OutlineLanguage::Rust,
        Some("ts") => OutlineLanguage::TypeScript,
        Some("tsx") => OutlineLanguage::Tsx,
        Some("js" | "mjs" | "cjs" | "jsx") => OutlineLanguage::JavaScript,
        Some("py" | "pyi") => OutlineLanguage::Python,
        Some("go") => OutlineLanguage::Go,
        Some("md" | "markdown") => OutlineLanguage::Markdown,
        _ => OutlineLanguage::Unsupported,
    }
}

fn parse_tree(source: &str, language: &Language) -> Result<Tree, ToolCallOutcome> {
    let mut parser = Parser::new();
    if let Err(e) = parser.set_language(language) {
        return Err(ToolCallOutcome::err(format!("failed to set language: {e}")));
    }

    parser
        .parse(source, None)
        .ok_or_else(|| ToolCallOutcome::err("failed to parse file"))
}

fn extract_cpp_outline(source: &str, include_private: bool) -> Result<String, ToolCallOutcome> {
    let language = tree_sitter_cpp::LANGUAGE.into();
    let tree = parse_tree(source, &language)?;

    let mut output = String::new();
    let mut ctx = OutlineContext {
        source,
        include_private,
        indent: 0,
    };

    extract_outline(tree.root_node(), &mut ctx, &mut output);
    Ok(output.trim().to_string())
}

fn extract_outline_with_tags_query(
    source: &str,
    outline_language: OutlineLanguage,
) -> Result<String, ToolCallOutcome> {
    let (language, tags_query, query_cache) =
        tags_query_spec(outline_language).expect("tags query language");
    let tree = parse_tree(source, &language)?;
    let query = cached_tags_query(query_cache, language, tags_query)?;
    Ok(extract_outline_via_tags_query(&tree, source, &query))
}

fn tags_query_spec(
    outline_language: OutlineLanguage,
) -> Option<(Language, &'static str, &'static OnceLock<CachedTagsQuery>)> {
    match outline_language {
        OutlineLanguage::Rust => Some((
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::TAGS_QUERY,
            &RUST_TAGS_QUERY,
        )),
        OutlineLanguage::TypeScript => Some((
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            tree_sitter_typescript::TAGS_QUERY,
            &TYPESCRIPT_TAGS_QUERY,
        )),
        OutlineLanguage::Tsx => Some((
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            tree_sitter_typescript::TAGS_QUERY,
            &TSX_TAGS_QUERY,
        )),
        OutlineLanguage::JavaScript => Some((
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::TAGS_QUERY,
            &JAVASCRIPT_TAGS_QUERY,
        )),
        OutlineLanguage::Python => Some((
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::TAGS_QUERY,
            &PYTHON_TAGS_QUERY,
        )),
        OutlineLanguage::Go => Some((
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::TAGS_QUERY,
            &GO_TAGS_QUERY,
        )),
        OutlineLanguage::Cpp | OutlineLanguage::Markdown | OutlineLanguage::Unsupported => None,
    }
}

fn cached_tags_query(
    query_cache: &'static OnceLock<CachedTagsQuery>,
    language: Language,
    tags_query: &'static str,
) -> Result<Arc<Query>, ToolCallOutcome> {
    match query_cache.get_or_init(|| {
        Query::new(&language, tags_query)
            .map(Arc::new)
            .map_err(|e| format!("failed to compile tags query: {e}"))
    }) {
        Ok(query) => Ok(Arc::clone(query)),
        Err(message) => Err(ToolCallOutcome::err(message.clone())),
    }
}

fn extract_outline_via_tags_query(tree: &Tree, source: &str, query: &Query) -> String {
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut output = String::new();

    while let Some(query_match) = matches.next() {
        let mut kind = None;
        let mut fallback_kind = None;
        let mut name = None;

        for capture in query_match.captures {
            let capture_name: &str = capture_names[capture.index as usize];
            if capture_name == "name" {
                let captured_name = node_text(capture.node, source).trim();
                if !captured_name.is_empty() {
                    name = Some(captured_name.to_string());
                }
                continue;
            }

            if fallback_kind.is_none() {
                fallback_kind = Some(capture_name);
            }

            if kind.is_none() && capture_name.starts_with("definition.") {
                kind = Some(capture_name);
            }
        }

        if let Some(name) = name {
            let kind = kind.or(fallback_kind).unwrap_or("definition");
            let _ = writeln!(output, "{kind} {name}");
        }
    }

    output.trim_end().to_string()
}

fn extract_markdown_outline(source: &str) -> String {
    let mut output = String::new();

    for line in source.lines() {
        let trimmed = line.trim_start();
        let depth = trimmed.chars().take_while(|&c| c == '#').count();
        if !(1..=6).contains(&depth) {
            continue;
        }

        let Some(rest) = trimmed.get(depth..) else {
            continue;
        };

        let Some(heading_text) = rest.strip_prefix(' ') else {
            continue;
        };

        let heading_text = heading_text.trim();
        if heading_text.is_empty() {
            continue;
        }

        output.push_str(&"  ".repeat(depth - 1));
        output.push_str("# ");
        output.push_str(heading_text);
        output.push('\n');
    }

    output.trim_end().to_string()
}

struct OutlineContext<'a> {
    source: &'a str,
    include_private: bool,
    indent: usize,
}

fn indent_str(level: usize) -> String {
    "    ".repeat(level)
}

fn node_text<'a>(node: Node<'a>, source: &'a str) -> &'a str {
    node.utf8_text(source.as_bytes()).unwrap_or("")
}

fn extract_outline(node: Node, ctx: &mut OutlineContext, output: &mut String) {
    match node.kind() {
        "preproc_include" => {
            output.push_str(&indent_str(ctx.indent));
            output.push_str(node_text(node, ctx.source).trim());
            output.push('\n');
        }

        "preproc_ifdef" | "preproc_ifndef" | "preproc_if" => {
            let text = node_text(node, ctx.source);
            if let Some(first_line) = text.lines().next() {
                output.push_str(&indent_str(ctx.indent));
                output.push_str(first_line.trim());
                output.push('\n');
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_outline(child, ctx, output);
            }
            output.push_str(&indent_str(ctx.indent));
            output.push_str("#endif\n");
        }

        "namespace_definition" => {
            let name = node
                .child_by_field_name("name")
                .map_or("anonymous", |n| node_text(n, ctx.source));

            let _ = writeln!(output, "namespace {name} {{");

            ctx.indent += 1;
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    extract_outline(child, ctx, output);
                }
            }
            ctx.indent -= 1;

            let _ = write!(output, "}} // namespace {name}\n\n");
        }

        "class_specifier" | "struct_specifier" => {
            let keyword = if node.kind() == "class_specifier" {
                "class"
            } else {
                "struct"
            };
            let name = node
                .child_by_field_name("name")
                .map_or("anonymous", |n| node_text(n, ctx.source));

            let mut base_clause = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "base_class_clause" {
                    base_clause = format!(" {}", node_text(child, ctx.source).trim());
                    break;
                }
            }

            if let Some(comment) = find_preceding_comment(node, ctx.source) {
                output.push_str(&indent_str(ctx.indent));
                output.push_str(comment.trim());
                output.push('\n');
            }

            let _ = writeln!(output, "{keyword} {name}{base_clause} {{");

            ctx.indent += 1;

            if let Some(body) = node.child_by_field_name("body") {
                extract_class_body(body, ctx, output, keyword == "struct");
            }

            ctx.indent -= 1;
            output.push_str(&indent_str(ctx.indent));
            output.push_str("};\n\n");
        }

        "enum_specifier" => {
            let name = node
                .child_by_field_name("name")
                .map_or("", |n| node_text(n, ctx.source));

            let text = node_text(node, ctx.source);
            let is_enum_class = text.contains("enum class") || text.contains("enum struct");

            if let Some(comment) = find_preceding_comment(node, ctx.source) {
                output.push_str(&indent_str(ctx.indent));
                output.push_str(comment.trim());
                output.push('\n');
            }

            output.push_str(&indent_str(ctx.indent));
            if is_enum_class {
                let _ = writeln!(output, "enum class {name} {{");
            } else {
                let _ = writeln!(output, "enum {name} {{");
            }

            if let Some(body) = node.child_by_field_name("body") {
                ctx.indent += 1;
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    if child.kind() == "enumerator" {
                        output.push_str(&indent_str(ctx.indent));
                        output.push_str(node_text(child, ctx.source).trim());
                        output.push_str(",\n");
                    }
                }
                ctx.indent -= 1;
            }

            output.push_str(&indent_str(ctx.indent));
            output.push_str("};\n\n");
        }

        "type_definition" | "alias_declaration" => {
            if let Some(comment) = find_preceding_comment(node, ctx.source) {
                output.push_str(&indent_str(ctx.indent));
                output.push_str(comment.trim());
                output.push('\n');
            }
            output.push_str(&indent_str(ctx.indent));
            output.push_str(node_text(node, ctx.source).trim());
            output.push('\n');
        }

        "declaration" => {
            let text = node_text(node, ctx.source);
            if !text.contains('=')
                || text.contains("= 0")
                || text.contains("= default")
                || text.contains("= delete")
            {
                if let Some(comment) = find_preceding_comment(node, ctx.source) {
                    output.push_str(&indent_str(ctx.indent));
                    output.push_str(comment.trim());
                    output.push('\n');
                }
                output.push_str(&indent_str(ctx.indent));
                output.push_str(text.trim());
                output.push('\n');
            }
        }

        "function_definition" => {
            let sig = extract_function_signature(node, ctx.source);
            if !sig.is_empty() {
                if let Some(comment) = find_preceding_comment(node, ctx.source) {
                    output.push_str(&indent_str(ctx.indent));
                    output.push_str(comment.trim());
                    output.push('\n');
                }
                output.push_str(&indent_str(ctx.indent));
                output.push_str(&sig);
                output.push_str(";\n");
            }
        }

        "template_declaration" => {
            let mut template_params = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "template_parameter_list" {
                    template_params = format!("template{}", node_text(child, ctx.source));
                    break;
                }
            }

            if !template_params.is_empty() {
                output.push_str(&indent_str(ctx.indent));
                output.push_str(&template_params);
                output.push('\n');
            }

            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() != "template_parameter_list" {
                    extract_outline(child, ctx, output);
                }
            }
        }

        "comment" => {}

        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_outline(child, ctx, output);
            }
        }
    }
}

fn extract_class_body(body: Node, ctx: &mut OutlineContext, output: &mut String, is_struct: bool) {
    let mut current_access = if is_struct { "public" } else { "private" };
    let mut has_explicit_access = false;
    let mut printed_current_access = false;

    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "access_specifier" => {
                output.push_str(&indent_str(ctx.indent - 1));
                output.push_str(node_text(child, ctx.source).trim());
                output.push('\n');

                let text = node_text(child, ctx.source).trim();
                current_access = if text.starts_with("public") {
                    "public"
                } else if text.starts_with("protected") {
                    "protected"
                } else {
                    "private"
                };
                has_explicit_access = true;
                printed_current_access = true;
            }

            "field_declaration"
            | "function_definition"
            | "declaration"
            | "alias_declaration"
            | "type_definition"
            | "enum_specifier"
            | "class_specifier"
            | "struct_specifier"
            | "template_declaration" => {
                if !ctx.include_private && current_access == "private" {
                    continue;
                }

                if !has_explicit_access && !printed_current_access && !ctx.include_private {
                    output.push_str(&indent_str(ctx.indent - 1));
                    output.push_str(current_access);
                    output.push_str(":\n");
                    printed_current_access = true;
                }

                match child.kind() {
                    "field_declaration" => {
                        if let Some(comment) = find_preceding_comment(child, ctx.source) {
                            output.push_str(&indent_str(ctx.indent));
                            output.push_str(comment.trim());
                            output.push('\n');
                        }
                        output.push_str(&indent_str(ctx.indent));
                        output.push_str(node_text(child, ctx.source).trim());
                        output.push('\n');
                    }
                    "function_definition" => {
                        let sig = extract_function_signature(child, ctx.source);
                        if !sig.is_empty() {
                            if let Some(comment) = find_preceding_comment(child, ctx.source) {
                                output.push_str(&indent_str(ctx.indent));
                                output.push_str(comment.trim());
                                output.push('\n');
                            }
                            output.push_str(&indent_str(ctx.indent));
                            output.push_str(&sig);
                            output.push_str(";\n");
                        }
                    }
                    "declaration" => {
                        let text = node_text(child, ctx.source);
                        if let Some(comment) = find_preceding_comment(child, ctx.source) {
                            output.push_str(&indent_str(ctx.indent));
                            output.push_str(comment.trim());
                            output.push('\n');
                        }
                        output.push_str(&indent_str(ctx.indent));
                        output.push_str(text.trim());
                        output.push('\n');
                    }
                    _ => {
                        extract_outline(child, ctx, output);
                    }
                }
            }

            "friend_declaration" => {
                if !has_explicit_access && !printed_current_access && !ctx.include_private {
                    output.push_str(&indent_str(ctx.indent - 1));
                    output.push_str(current_access);
                    output.push_str(":\n");
                    printed_current_access = true;
                }
                output.push_str(&indent_str(ctx.indent));
                output.push_str(node_text(child, ctx.source).trim());
                output.push('\n');
            }

            _ => {}
        }
    }
}

fn extract_function_signature(node: Node, source: &str) -> String {
    let full_text = node_text(node, source);

    let mut body_pos = None;
    let mut init_list_pos = None;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "compound_statement" => {
                body_pos = Some(child.start_byte() - node.start_byte());
            }
            "field_initializer_list" => {
                init_list_pos = Some(child.start_byte() - node.start_byte());
            }
            _ => {}
        }
    }

    let cut_pos = init_list_pos.or(body_pos);

    if let Some(pos) = cut_pos {
        let mut sig = full_text[..pos].trim_end();

        if init_list_pos.is_some() {
            sig = sig.trim_end_matches(':').trim_end();
        }

        sig.to_string()
    } else {
        full_text.trim_end_matches(';').trim().to_string()
    }
}

fn find_preceding_comment<'a>(node: Node<'a>, source: &'a str) -> Option<&'a str> {
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "comment" {
            let text = node_text(p, source);
            if text.starts_with("///") || text.starts_with("/**") {
                return Some(text);
            }
        } else if p.kind() != "preproc_call" {
            break;
        }
        prev = p.prev_sibling();
    }
    None
}

define_mcp_tool! {
    OutlineTool,
    name: "Outline",
    description: "Extract a structural outline (declarations, headings) from a source file. Supports: C++ (.cpp/.cxx/.cc/.h/.hpp/.hxx), Rust (.rs), TypeScript (.ts/.tsx), JavaScript (.js/.mjs/.cjs/.jsx), Python (.py/.pyi), Go (.go), Markdown (.md/.markdown).",
    schema: {
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the source file to outline"
            },
            "include_private": {
                "type": "boolean",
                "description": "Include private members in output for C++ files; ignored for other languages"
            }
        },
        "required": ["path"],
        "additionalProperties": false
    },
    handler: handle_outline
}

#[cfg(test)]
mod tests {
    use super::{
        OutlineLanguage, SUPPORTED_OUTLINE_EXTENSIONS, cached_tags_query, extract_markdown_outline,
        render_outline, tags_query_spec,
    };
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn each_supported_language_emits_at_least_one_entry() {
        let cases = [
            (
                "sample.cpp",
                "class Greeter {\npublic:\n    void greet();\n};\n",
                false,
            ),
            ("sample.rs", "fn greet() {}\n", false),
            (
                "sample.ts",
                "interface Greeter {\n    greet(): void;\n}\n",
                false,
            ),
            (
                "sample.tsx",
                "abstract class App {\n    abstract render(): JSX.Element;\n}\nconst view = <div />;\n",
                false,
            ),
            ("sample.js", "function greet() {}\n", false),
            ("sample.py", "def greet():\n    return 'hi'\n", false),
            ("sample.go", "package demo\n\nfunc Greet() {}\n", false),
            ("sample.md", "# Heading\n", false),
        ];

        for (path, source, include_private) in cases {
            let outline = render_outline(Path::new(path), source, include_private)
                .unwrap_or_else(|_| panic!("expected outline for {path}"));
            assert!(
                !outline.trim().is_empty(),
                "expected {path} to emit at least one outline entry"
            );
        }
    }

    #[test]
    fn unsupported_extension_returns_structured_error() {
        let outcome = render_outline(Path::new("sample.txt"), "plain text\n", false)
            .expect_err("unsupported extension should fail");

        assert_eq!(outcome.0["isError"], true);
        assert_eq!(
            outcome.0["content"][0]["text"],
            "unsupported language for outline"
        );
        assert_eq!(outcome.0["path"], "sample.txt");
        assert_eq!(outcome.0["extension"], "txt");
        assert_eq!(outcome.0["supported"], json!(SUPPORTED_OUTLINE_EXTENSIONS));
    }

    #[test]
    fn repeated_tags_query_outline_extraction_returns_identical_output() {
        let source = "pub struct Greeter;\n\npub fn greet() {}\n";

        let first =
            render_outline(Path::new("sample.rs"), source, false).expect("first rust outline");
        let second =
            render_outline(Path::new("sample.rs"), source, false).expect("second rust outline");

        assert_eq!(second, first);
    }

    #[test]
    fn tags_query_cache_reuses_compiled_query_for_language_variant() {
        let (language, tags_query, query_cache) =
            tags_query_spec(OutlineLanguage::Rust).expect("rust tags query spec");

        let first =
            cached_tags_query(query_cache, language, tags_query).expect("compile rust tags query");
        let (language, tags_query, query_cache) =
            tags_query_spec(OutlineLanguage::Rust).expect("rust tags query spec");
        let second =
            cached_tags_query(query_cache, language, tags_query).expect("reuse rust tags query");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn markdown_heading_extraction_respects_depth() {
        let outline = extract_markdown_outline(
            "# Root\n## Child\n### Grandchild\n#### Great Grandchild\n###Ignored\n",
        );

        assert_eq!(
            outline,
            "# Root\n  # Child\n    # Grandchild\n      # Great Grandchild"
        );
    }

    mod cache {
        use super::super::{OutlineLanguage, outline_content_hash, outline_for_path};
        use crate::path_policy::tempdir_in_workspace;
        use crate::tools::scope_cache::{OutlineKey, outline_ast_cache};
        use std::fs;
        use std::path::Path;
        use std::sync::Arc;

        fn key_for(path: &Path, lang: OutlineLanguage, include_private: bool) -> OutlineKey {
            let meta = fs::metadata(path).expect("metadata");
            OutlineKey {
                path: path.to_path_buf(),
                language: lang
                    .cache_language(include_private)
                    .expect("supported language"),
                modified: meta.modified().ok(),
                len: meta.len(),
                content_hash: outline_content_hash(fs::read(path).expect("read source").as_slice()),
            }
        }

        fn outline_text(outcome: &tools_mcp_core::ToolCallOutcome) -> String {
            outcome.0["content"][0]["text"]
                .as_str()
                .expect("content text")
                .to_string()
        }

        #[tokio::test]
        async fn rust_outline_populates_cache_and_returns_identical_second_call() {
            let dir = tempdir_in_workspace("outline-cache-rust-");
            let path = dir.path().join("sample.rs");
            fs::write(&path, "fn greet() {}\n").expect("write rust source");
            let path_str = path.to_string_lossy().to_string();

            let first = outline_for_path(&path_str, false).await;
            assert_eq!(first.0["isError"], false, "first call should succeed");
            let first_text = outline_text(&first);

            let key = key_for(&path, OutlineLanguage::Rust, false);
            let cached = outline_ast_cache()
                .get(&key)
                .expect("rust outline should be cached");
            assert_eq!(cached.as_str(), first_text);

            let second = outline_for_path(&path_str, false).await;
            assert_eq!(second.0["isError"], false, "second call should succeed");
            assert_eq!(outline_text(&second), first_text);
        }

        #[tokio::test]
        async fn cache_invalidates_when_file_changes() {
            let dir = tempdir_in_workspace("outline-cache-mtime-");
            let path = dir.path().join("sample.rs");
            fs::write(&path, "fn first() {}\n").expect("write initial source");
            let path_str = path.to_string_lossy().to_string();

            let initial = outline_for_path(&path_str, false).await;
            let initial_text = outline_text(&initial);
            assert!(initial_text.contains("first"));

            // Force a distinguishable mtime/len so the key changes even on
            // filesystems with coarse-grained timestamps.
            std::thread::sleep(std::time::Duration::from_millis(20));
            fs::write(&path, "fn second_renamed_symbol() {}\n")
                .expect("rewrite source with longer body");

            let refreshed = outline_for_path(&path_str, false).await;
            let refreshed_text = outline_text(&refreshed);
            assert!(
                refreshed_text.contains("second_renamed_symbol"),
                "expected fresh outline to reflect new content, got: {refreshed_text}"
            );
            assert_ne!(initial_text, refreshed_text);
        }

        #[tokio::test]
        async fn cache_invalidates_same_len_same_mtime_when_content_hash_changes() {
            let dir = tempdir_in_workspace("outline-cache-hash-");
            let path = dir.path().join("sample.rs");
            let first_source = "fn alpha() {}\n";
            let second_source = "fn bravo() {}\n";
            assert_eq!(first_source.len(), second_source.len());

            fs::write(&path, first_source).expect("write initial source");
            let path_str = path.to_string_lossy().to_string();
            let initial = outline_for_path(&path_str, false).await;
            let initial_text = outline_text(&initial);
            assert!(initial_text.contains("alpha"));

            fs::write(&path, second_source).expect("write current source");
            let meta = fs::metadata(&path).expect("metadata");
            let stale_key = OutlineKey {
                path: path.to_path_buf(),
                language: OutlineLanguage::Rust
                    .cache_language(false)
                    .expect("supported language"),
                modified: meta.modified().ok(),
                len: meta.len(),
                content_hash: outline_content_hash(first_source.as_bytes()),
            };
            outline_ast_cache().insert(stale_key, Arc::new(initial_text));

            let refreshed = outline_for_path(&path_str, false).await;
            assert_eq!(refreshed.0["isError"], false);
            let refreshed_text = outline_text(&refreshed);
            assert!(
                refreshed_text.contains("bravo"),
                "expected fresh outline to reflect current content, got: {refreshed_text}"
            );
            assert!(
                !refreshed_text.contains("alpha"),
                "stale cache entry should not be reused: {refreshed_text}"
            );
        }

        #[tokio::test]
        async fn markdown_outline_populates_cache() {
            let dir = tempdir_in_workspace("outline-cache-md-");
            let path = dir.path().join("notes.md");
            fs::write(&path, "# Heading\n## Sub\n").expect("write markdown");
            let path_str = path.to_string_lossy().to_string();

            let outcome = outline_for_path(&path_str, false).await;
            assert_eq!(outcome.0["isError"], false);

            let key = key_for(&path, OutlineLanguage::Markdown, false);
            assert!(
                outline_ast_cache().get(&key).is_some(),
                "markdown outline should be cached"
            );
        }

        #[tokio::test]
        async fn unsupported_extension_does_not_touch_cache() {
            let dir = tempdir_in_workspace("outline-cache-unsupported-");
            let path = dir.path().join("readme.txt");
            fs::write(&path, "plain text\n").expect("write txt");
            let path_str = path.to_string_lossy().to_string();

            // Seed the cache with a sentinel keyed on this path under a fake
            // language so we can detect any accidental writes by the handler.
            let meta = fs::metadata(&path).expect("metadata");
            let sentinel_key = OutlineKey {
                path: path.to_path_buf(),
                language: "sentinel-unsupported".to_string(),
                modified: meta.modified().ok(),
                len: meta.len(),
                content_hash: outline_content_hash(b"plain text\n"),
            };
            let sentinel_value = Arc::new("__sentinel__".to_string());
            outline_ast_cache().insert(sentinel_key.clone(), sentinel_value.clone());

            let outcome = outline_for_path(&path_str, false).await;
            assert_eq!(
                outcome.0["isError"], true,
                "unsupported extension should error"
            );
            assert_eq!(
                outcome.0["content"][0]["text"],
                "unsupported language for outline"
            );
            assert_eq!(outcome.0["extension"], "txt");

            // The sentinel must still be present unchanged (handler must not
            // have written its own entry for the path).
            let still = outline_ast_cache()
                .get(&sentinel_key)
                .expect("sentinel must remain");
            assert_eq!(still.as_str(), sentinel_value.as_str());
        }
    }
}
