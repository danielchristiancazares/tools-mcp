use crate::RpcResponse;
use crate::define_mcp_tool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use tree_sitter::{Node, Parser};

#[derive(Deserialize)]
struct OutlineRequest {
    path: String,
    #[serde(default)]
    include_private: Option<bool>,
}

async fn handle_outline(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<OutlineRequest>(id.clone(), args) {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let path = Path::new(&req.path);
    if !path.exists() {
        return RpcResponse::err(id, format!("file not found: {}", path.display()));
    }

    let source = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) => {
            return RpcResponse::err(id, format!("failed to read file: {e}"));
        }
    };

    let include_private = req.include_private.unwrap_or(false);

    let mut parser = Parser::new();
    let language = tree_sitter_cpp::LANGUAGE;
    if let Err(e) = parser.set_language(&language.into()) {
        return RpcResponse::err(id, format!("failed to set language: {e}"));
    }

    let tree = match parser.parse(&source, None) {
        Some(t) => t,
        None => {
            return RpcResponse::err(id, "failed to parse file");
        }
    };

    let mut output = String::new();
    let mut ctx = OutlineContext {
        source: &source,
        include_private,
        indent: 0,
    };

    extract_outline(tree.root_node(), &mut ctx, &mut output);

    let payload = json!({
        "content": [{"type": "text", "text": output.trim()}],
        "isError": false,
        "path": req.path,
        "bytes": source.len(),
        "outline_bytes": output.len(),
    });

    RpcResponse::ok(id, payload)
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
                .map(|n| node_text(n, ctx.source))
                .unwrap_or("anonymous");

            output.push_str(&indent_str(ctx.indent));
            output.push_str(&format!("namespace {} {{\n", name));

            ctx.indent += 1;
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    extract_outline(child, ctx, output);
                }
            }
            ctx.indent -= 1;

            output.push_str(&indent_str(ctx.indent));
            output.push_str(&format!("}} // namespace {}\n\n", name));
        }

        "class_specifier" | "struct_specifier" => {
            let keyword = if node.kind() == "class_specifier" {
                "class"
            } else {
                "struct"
            };
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, ctx.source))
                .unwrap_or("anonymous");

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

            output.push_str(&indent_str(ctx.indent));
            output.push_str(&format!("{} {}{} {{\n", keyword, name, base_clause));

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
                .map(|n| node_text(n, ctx.source))
                .unwrap_or("");

            let text = node_text(node, ctx.source);
            let is_enum_class = text.contains("enum class") || text.contains("enum struct");

            if let Some(comment) = find_preceding_comment(node, ctx.source) {
                output.push_str(&indent_str(ctx.indent));
                output.push_str(comment.trim());
                output.push('\n');
            }

            output.push_str(&indent_str(ctx.indent));
            if is_enum_class {
                output.push_str(&format!("enum class {} {{\n", name));
            } else {
                output.push_str(&format!("enum {} {{\n", name));
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

        "type_definition" => {
            if let Some(comment) = find_preceding_comment(node, ctx.source) {
                output.push_str(&indent_str(ctx.indent));
                output.push_str(comment.trim());
                output.push('\n');
            }
            output.push_str(&indent_str(ctx.indent));
            output.push_str(node_text(node, ctx.source).trim());
            output.push('\n');
        }

        "alias_declaration" => {
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
            if let Some(sig) = extract_function_signature(node, ctx.source) {
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
                        if let Some(sig) = extract_function_signature(child, ctx.source) {
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
                    "template_declaration" => {
                        extract_outline(child, ctx, output);
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

fn extract_function_signature(node: Node, source: &str) -> Option<String> {
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

        Some(sig.to_string())
    } else {
        Some(full_text.trim_end_matches(';').trim().to_string())
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
    aliases: ["outline"],
    description: "Extract structural outline from C++ source code",
    schema: {
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the C++ file to outline"
            },
            "include_private": {
                "type": "boolean",
                "description": "Include private members in output"
            }
        },
        "required": ["path"],
        "additionalProperties": false
    },
    handler: handle_outline
}
