use crate::rustverify::{ParsedRust, RustAnalyzer};
use crate::{err_text, RpcResponse};
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

#[derive(Deserialize)]
struct RustCallGraphRequest {
    #[serde(default)]
    file_paths: Option<Vec<String>>,
    #[serde(default)]
    root_dir: Option<String>,
}

/// MCP handler for the RustCallGraph tool.
pub async fn handle_rust_callgraph(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match serde_json::from_value::<RustCallGraphRequest>(args) {
        Ok(req) => req,
        Err(e) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!("invalid arguments: {}", e))),
                error: None,
            }
        }
    };

    let files = match resolve_file_paths(&req) {
        Ok(list) => list,
        Err(e) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!("RustCallGraph error: {:#}", e))),
                error: None,
            }
        }
    };

    if files.is_empty() {
        return RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(err_text(
                "RustCallGraph did not find any Rust source files to analyze",
            )),
            error: None,
        };
    }

    let analyzer = RustAnalyzer::new();
    let mut parsed_files = Vec::new();

    for path in &files {
        let p = Path::new(path);
        match analyzer.parse_file(p) {
            Ok(parsed) => parsed_files.push(parsed),
            Err(e) => {
                return RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(err_text(&format!(
                        "failed to parse Rust file '{}': {:#}",
                        p.display(),
                        e
                    ))),
                    error: None,
                }
            }
        }
    }

    let graph = match compute_call_graph(&parsed_files) {
        Ok(g) => g,
        Err(e) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!("RustCallGraph error: {:#}", e))),
                error: None,
            }
        }
    };

    let text = serde_json::to_string_pretty(&graph).unwrap_or_else(|_| "{}".to_string());

    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(json!({
            "content": [{
                "type": "text",
                "text": text
            }],
            "isError": false
        })),
        error: None,
    }
}

fn resolve_file_paths(req: &RustCallGraphRequest) -> Result<Vec<String>> {
    if let Some(explicit) = req.file_paths.as_ref() {
        let filtered: Vec<String> = explicit
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if !filtered.is_empty() {
            return Ok(filtered);
        }
    }

    let root: PathBuf = if let Some(dir) = req.root_dir.as_ref() {
        PathBuf::from(dir)
    } else {
        std::env::current_dir().context("RustCallGraph could not determine current_dir")?
    };

    discover_rust_files(&root)
}

const SKIP_DIRS: &[&str] = &[".git", ".hg", ".svn", ".idea", ".vscode", "target", "node_modules"];

fn should_visit(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }

    if let Some(name) = entry.file_name().to_str() {
        if entry.file_type().is_dir() {
            let lower = name.to_ascii_lowercase();
            if SKIP_DIRS.contains(&lower.as_str()) {
                return false;
            }
            if lower.starts_with('.') {
                return false;
            }
        }
    }

    true
}

fn discover_rust_files(root: &Path) -> Result<Vec<String>> {
    let mut results = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| should_visit(e))
    {
        let entry = entry.context("walk directory")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            if ext.eq_ignore_ascii_case("rs") {
                results.push(path.to_string_lossy().to_string());
            }
        }
    }

    Ok(results)
}

fn compute_call_graph(parsed_files: &[ParsedRust]) -> Result<Value> {
    use syn::{visit::Visit, Expr, ExprCall};

    #[derive(Clone)]
    struct Node {
        id: String,
        name: String,
        file: String,
    }

    let mut nodes = Vec::new();

    for parsed in parsed_files {
        let file = parsed.file_path.clone();
        let funcs = RustAnalyzer::extract_functions(parsed);
        for func in funcs {
            let name = func.sig.ident.to_string();
            let id = format!("{}::{}", file, name);
            nodes.push(Node {
                id,
                name,
                file: file.clone(),
            });
        }
    }

    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    for node in &nodes {
        by_name
            .entry(node.name.clone())
            .or_default()
            .push(node.id.clone());
    }

    struct CallVisitor<'a> {
        current_id: &'a str,
        out: &'a mut Vec<(String, String)>,
    }

    impl<'a, 'ast> Visit<'ast> for CallVisitor<'a> {
        fn visit_expr_call(&mut self, node: &'ast ExprCall) {
            if let Expr::Path(ref p) = *node.func {
                if let Some(seg) = p.path.segments.last() {
                    let callee = seg.ident.to_string();
                    self.out.push((self.current_id.to_string(), callee));
                }
            }
            syn::visit::visit_expr_call(self, node);
        }
    }

    let mut raw_calls: Vec<(String, String)> = Vec::new();

    for parsed in parsed_files {
        let file = parsed.file_path.clone();
        let funcs = RustAnalyzer::extract_functions(parsed);
        for func in funcs {
            let name = func.sig.ident.to_string();
            let id = format!("{}::{}", file, name);
            let mut visitor = CallVisitor {
                current_id: &id,
                out: &mut raw_calls,
            };
            visitor.visit_item_fn(func);
        }
    }

    let mut edges = Vec::new();
    for (from, callee_name) in raw_calls {
        if let Some(targets) = by_name.get(&callee_name) {
            if targets.len() == 1 {
                edges.push(json!({
                    "from": from,
                    "to": targets[0],
                    "call": callee_name
                }));
            }
        }
    }

    let mut node_values = Vec::new();
    for node in nodes {
        node_values.push(json!({
            "id": node.id,
            "name": node.name,
            "file": node.file
        }));
    }

    Ok(json!({
        "nodes": node_values,
        "edges": edges
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_compute_call_graph_simple() {
        let source = r#"
            fn a() {
                b();
            }

            fn b() {}
        "#;

        let analyzer = RustAnalyzer::new();
        let parsed = analyzer
            .parse_str(source, "test.rs")
            .expect("parse in-memory code");

        let graph = compute_call_graph(&[parsed]).expect("graph");
        let edges = graph
            .get("edges")
            .and_then(|v| v.as_array())
            .expect("edges array");

        assert!(
            edges
                .iter()
                .any(|e| e.get("call").and_then(|v| v.as_str()) == Some("b")),
            "expected an edge calling 'b'"
        );
    }
}

