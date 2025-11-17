// Rust source code parser using syn crate

use anyhow::{Context, Result};
use proc_macro2;
use std::fs;
use std::path::Path;
use syn::{File as SynFile, Item};

/// Parsed Rust source code with AST representation
#[derive(Debug, Clone)]
pub struct ParsedRust {
    /// Original source code
    pub source: String,
    /// Parsed AST
    pub ast: SynFile,
    /// File path
    pub file_path: String,
}

/// Rust source code analyzer using syn
pub struct RustAnalyzer;

impl RustAnalyzer {
    pub fn new() -> Self {
        Self
    }

    /// Parse a Rust source file into an AST
    pub fn parse_file(&self, path: &Path) -> Result<ParsedRust> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        let ast = syn::parse_file(&source)
            .with_context(|| format!("Failed to parse Rust file: {}", path.display()))?;

        Ok(ParsedRust {
            source,
            ast,
            file_path: path.display().to_string(),
        })
    }

    /// Parse Rust source code from a string
    pub fn parse_str(&self, source: &str, file_path: &str) -> Result<ParsedRust> {
        let ast = syn::parse_file(source)
            .with_context(|| format!("Failed to parse Rust code from: {}", file_path))?;

        Ok(ParsedRust {
            source: source.to_string(),
            ast,
            file_path: file_path.to_string(),
        })
    }

    /// Extract all function items from the AST
    pub fn extract_functions(parsed: &ParsedRust) -> Vec<&syn::ItemFn> {
        let mut functions = Vec::new();

        for item in &parsed.ast.items {
            if let Item::Fn(func) = item {
                functions.push(func);
            }
        }

        functions
    }

    /// Get line and column from a syn::Span
    pub fn get_location(_source: &str, _span: proc_macro2::Span) -> (usize, usize) {
        // TODO: Implement proper span location extraction
        // For MVP, return placeholder
        (1, 1)
    }
}

impl Default for RustAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_function() {
        let analyzer = RustAnalyzer::new();
        let source = r#"
            fn add(a: i32, b: i32) -> i32 {
                a + b
            }
        "#;

        let parsed = analyzer.parse_str(source, "test.rs").unwrap();
        assert_eq!(parsed.file_path, "test.rs");

        let functions = RustAnalyzer::extract_functions(&parsed);
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].sig.ident, "add");
    }

    #[test]
    fn test_parse_division_by_zero() {
        let analyzer = RustAnalyzer::new();
        let source = r#"
            fn divide(x: i32, y: i32) -> i32 {
                x / y
            }
        "#;

        let parsed = analyzer.parse_str(source, "test.rs").unwrap();
        let functions = RustAnalyzer::extract_functions(&parsed);
        assert_eq!(functions.len(), 1);
    }

    #[test]
    fn test_parse_invalid_syntax() {
        let analyzer = RustAnalyzer::new();
        let source = "fn broken {{{";

        let result = analyzer.parse_str(source, "test.rs");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_multiple_functions() {
        let analyzer = RustAnalyzer::new();
        let source = r#"
            fn foo() -> i32 { 1 }
            fn bar() -> i32 { 2 }
            fn baz() -> i32 { 3 }
        "#;

        let parsed = analyzer.parse_str(source, "test.rs").unwrap();
        let functions = RustAnalyzer::extract_functions(&parsed);
        assert_eq!(functions.len(), 3);
    }
}
