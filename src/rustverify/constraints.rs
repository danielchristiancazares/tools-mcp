// Constraint extraction from Rust AST for Z3 verification

use crate::rustverify::llm::LlmHypothesis;
use crate::rustverify::parser::ParsedRust;
use anyhow::Result;
use syn::visit::Visit;
use syn::{BinOp, Expr, ExprBinary, Local, Stmt};

/// A constraint that can be encoded in Z3
#[derive(Debug, Clone)]
pub enum Constraint {
    /// Variable declaration with optional initial value
    VarDecl {
        name: String,
        value: Option<ConstraintExpr>,
    },
    /// Binary operation constraint (e.g., x > 0)
    BinOp {
        left: ConstraintExpr,
        op: ConstraintOp,
        right: ConstraintExpr,
    },
    /// Assert that an expression must be true
    Assert(ConstraintExpr),
    /// Check if denominator can be zero
    DivisionCheck {
        numerator: ConstraintExpr,
        denominator: ConstraintExpr,
    },
}

#[derive(Debug, Clone)]
pub enum ConstraintExpr {
    Variable(String),
    Literal(i64),
    BinOp {
        left: Box<ConstraintExpr>,
        op: ConstraintOp,
        right: Box<ConstraintExpr>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum ConstraintOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// Extracts constraints from Rust code for verification
pub struct ConstraintExtractor;

impl ConstraintExtractor {
    pub fn new() -> Self {
        Self
    }

    /// Extract constraints relevant to a specific hypothesis
    pub fn extract(
        &self,
        parsed: &ParsedRust,
        hypothesis: &LlmHypothesis,
    ) -> Result<Vec<Constraint>> {
        // TODO: Use hypothesis.line to find the specific function
        // For MVP, we analyze ONLY the first function to avoid conflicting constraints
        let functions = crate::rustverify::parser::RustAnalyzer::extract_functions(parsed);

        let mut constraints = Vec::new();

        // Only analyze the first function for MVP
        if let Some(func) = functions.first() {
            // Add function parameters as variable declarations
            for param in &func.sig.inputs {
                if let syn::FnArg::Typed(pat_type) = param {
                    if let syn::Pat::Ident(pat_ident) = &*pat_type.pat {
                        constraints.push(Constraint::VarDecl {
                            name: pat_ident.ident.to_string(),
                            value: None, // Parameters are unconstrained
                        });
                    }
                }
            }

            // Extract constraints from function body
            let mut visitor = ConstraintVisitor {
                constraints: Vec::new(),
                hypothesis,
            };
            visitor.visit_item_fn(func);
            constraints.extend(visitor.constraints);
        }

        // Debug: print constraints
        eprintln!("DEBUG: Extracted {} constraints for function:", constraints.len());
        for (i, c) in constraints.iter().enumerate() {
            eprintln!("  [{}] {:?}", i, c);
        }

        Ok(constraints)
    }
}

impl Default for ConstraintExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// AST visitor that collects constraints
struct ConstraintVisitor<'a> {
    constraints: Vec<Constraint>,
    hypothesis: &'a LlmHypothesis,
}

impl<'a> ConstraintVisitor<'a> {
    fn extract_expr(&self, expr: &Expr) -> Option<ConstraintExpr> {
        match expr {
            Expr::Path(path) => {
                // Variable reference
                let ident = path.path.get_ident()?;
                Some(ConstraintExpr::Variable(ident.to_string()))
            }
            Expr::Lit(lit) => {
                // Literal value
                if let syn::Lit::Int(int_lit) = &lit.lit {
                    let value = int_lit.base10_parse::<i64>().ok()?;
                    Some(ConstraintExpr::Literal(value))
                } else {
                    None
                }
            }
            Expr::Binary(ExprBinary {
                left, op, right, ..
            }) => {
                let left_expr = Box::new(self.extract_expr(left)?);
                let right_expr = Box::new(self.extract_expr(right)?);
                let constraint_op = self.convert_binop(op)?;

                Some(ConstraintExpr::BinOp {
                    left: left_expr,
                    op: constraint_op,
                    right: right_expr,
                })
            }
            _ => None,
        }
    }

    fn convert_binop(&self, op: &BinOp) -> Option<ConstraintOp> {
        match op {
            BinOp::Add(_) => Some(ConstraintOp::Add),
            BinOp::Sub(_) => Some(ConstraintOp::Sub),
            BinOp::Mul(_) => Some(ConstraintOp::Mul),
            BinOp::Div(_) => Some(ConstraintOp::Div),
            BinOp::Rem(_) => Some(ConstraintOp::Mod),
            BinOp::Eq(_) => Some(ConstraintOp::Eq),
            BinOp::Ne(_) => Some(ConstraintOp::Ne),
            BinOp::Lt(_) => Some(ConstraintOp::Lt),
            BinOp::Le(_) => Some(ConstraintOp::Le),
            BinOp::Gt(_) => Some(ConstraintOp::Gt),
            BinOp::Ge(_) => Some(ConstraintOp::Ge),
            BinOp::And(_) => Some(ConstraintOp::And),
            BinOp::Or(_) => Some(ConstraintOp::Or),
            _ => None,
        }
    }
}

impl<'a> Visit<'a> for ConstraintVisitor<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Local(Local { pat, init, .. }) => {
                // Variable declaration
                if let syn::Pat::Ident(pat_ident) = pat {
                    let var_name = pat_ident.ident.to_string();
                    let value = init.as_ref().and_then(|i| self.extract_expr(&i.expr));

                    self.constraints.push(Constraint::VarDecl {
                        name: var_name,
                        value,
                    });
                }
            }
            _ => {}
        }

        syn::visit::visit_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        // Look for division operations
        if let Expr::Binary(ExprBinary {
            left,
            op: BinOp::Div(_) | BinOp::Rem(_),
            right,
            ..
        }) = expr
        {
            if let (Some(num), Some(den)) = (self.extract_expr(left), self.extract_expr(right)) {
                self.constraints.push(Constraint::DivisionCheck {
                    numerator: num,
                    denominator: den,
                });
            }
        }

        // Look for comparison operations in conditionals
        if let Expr::Binary(ExprBinary {
            left, op, right, ..
        }) = expr
        {
            if let Some(constraint_op) = self.convert_binop(op) {
                if matches!(
                    constraint_op,
                    ConstraintOp::Eq
                        | ConstraintOp::Ne
                        | ConstraintOp::Lt
                        | ConstraintOp::Le
                        | ConstraintOp::Gt
                        | ConstraintOp::Ge
                ) {
                    if let (Some(left_expr), Some(right_expr)) =
                        (self.extract_expr(left), self.extract_expr(right))
                    {
                        self.constraints.push(Constraint::BinOp {
                            left: left_expr,
                            op: constraint_op,
                            right: right_expr,
                        });
                    }
                }
            }
        }

        syn::visit::visit_expr(self, expr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rustverify::{AnalysisType, Severity};

    #[test]
    fn test_extract_division_constraint() {
        let source = r#"
            fn divide(x: i32, y: i32) -> i32 {
                x / y
            }
        "#;

        let analyzer = crate::rustverify::parser::RustAnalyzer::new();
        let parsed = analyzer.parse_str(source, "test.rs").unwrap();

        let hypothesis = LlmHypothesis {
            analysis_type: AnalysisType::DivisionByZero,
            line: 3,
            column: 17,
            severity: Severity::High,
            description: "Division by zero possible".to_string(),
            recommendation: "Add check for zero".to_string(),
        };

        let extractor = ConstraintExtractor::new();
        let constraints = extractor.extract(&parsed, &hypothesis).unwrap();

        assert!(!constraints.is_empty());
        assert!(matches!(
            constraints
                .iter()
                .any(|c| matches!(c, Constraint::DivisionCheck { .. })),
            true
        ));
    }

    #[test]
    fn test_extract_variable_declarations() {
        let source = r#"
            fn test() {
                let x = 5;
                let y = 10;
            }
        "#;

        let analyzer = crate::rustverify::parser::RustAnalyzer::new();
        let parsed = analyzer.parse_str(source, "test.rs").unwrap();

        let hypothesis = LlmHypothesis {
            analysis_type: AnalysisType::Logic,
            line: 3,
            column: 17,
            severity: Severity::Low,
            description: "Test".to_string(),
            recommendation: "None".to_string(),
        };

        let extractor = ConstraintExtractor::new();
        let constraints = extractor.extract(&parsed, &hypothesis).unwrap();

        let var_decls: Vec<_> = constraints
            .iter()
            .filter(|c| matches!(c, Constraint::VarDecl { .. }))
            .collect();

        assert_eq!(var_decls.len(), 2);
    }
}
