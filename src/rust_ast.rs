use crate::rustverify::RustAnalyzer;
use crate::{err_text, RpcResponse};
use anyhow::Result;
use quote::ToTokens;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Deserialize)]
struct RustAstRequest {
    file_path: String,
}

/// MCP handler for the RustAst tool.
pub async fn handle_rust_ast(id: Option<Value>, args: Value) -> RpcResponse<'static> {
    let req = match serde_json::from_value::<RustAstRequest>(args) {
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

    let path = Path::new(&req.file_path);
    let analyzer = RustAnalyzer::new();
    let parsed = match analyzer.parse_file(path) {
        Ok(p) => p,
        Err(e) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!(
                    "failed to parse Rust file '{}': {:#}",
                    path.display(),
                    e
                ))),
                error: None,
            }
        }
    };

    let payload = match summarize_parsed(&parsed) {
        Ok(v) => v,
        Err(e) => {
            return RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(err_text(&format!("RustAst error: {:#}", e))),
                error: None,
            }
        }
    };

    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());

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

fn summarize_parsed(parsed: &crate::rustverify::ParsedRust) -> Result<Value> {
    use syn::Item;

    let mut functions = Vec::new();
    let mut types = Vec::new();

    let funcs = crate::rustverify::RustAnalyzer::extract_functions(parsed);
    for func in funcs {
        let name = func.sig.ident.to_string();
        let visibility = match func.vis {
            syn::Visibility::Public(_) => "pub",
            _ => "private",
        };

        let signature = func.sig.to_token_stream().to_string();
        let is_async = func.sig.asyncness.is_some();
        let is_const = func.sig.constness.is_some();
        let is_unsafe = func.sig.unsafety.is_some();

        functions.push(json!({
            "name": name,
            "visibility": visibility,
            "signature": signature,
            "async": is_async,
            "const": is_const,
            "unsafe": is_unsafe
        }));
    }

    for item in &parsed.ast.items {
        match item {
            Item::Struct(s) => {
                types.push(json!({
                    "kind": "struct",
                    "name": s.ident.to_string()
                }));
            }
            Item::Enum(e) => {
                types.push(json!({
                    "kind": "enum",
                    "name": e.ident.to_string()
                }));
            }
            Item::Trait(t) => {
                types.push(json!({
                    "kind": "trait",
                    "name": t.ident.to_string()
                }));
            }
            _ => {}
        }
    }

    Ok(json!({
        "file": parsed.file_path,
        "functions": functions,
        "types": types
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rustverify::RustAnalyzer;

    #[tokio::test]
    async fn test_summarize_simple_file() {
        let source = r#"
            pub struct Foo;

            fn hidden() {}

            pub async fn visible(a: i32, b: i32) -> i32 {
                a + b
            }
        "#;

        let analyzer = RustAnalyzer::new();
        let parsed = analyzer
            .parse_str(source, "test.rs")
            .expect("parse in-memory source");

        let payload = summarize_parsed(&parsed).expect("summarize");
        let funcs = payload
            .get("functions")
            .and_then(|v| v.as_array())
            .expect("functions array");
        assert_eq!(funcs.len(), 2);

        let types = payload
            .get("types")
            .and_then(|v| v.as_array())
            .expect("types array");
        assert_eq!(types.len(), 1);
    }
}
