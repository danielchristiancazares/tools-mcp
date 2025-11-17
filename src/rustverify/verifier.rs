// Z3 SMT solver integration for formal verification

use crate::rustverify::constraints::{Constraint, ConstraintExpr, ConstraintOp};
use crate::rustverify::llm::LlmHypothesis;
use crate::rustverify::VerificationStatus;
use anyhow::{Context, Result};
use std::collections::HashMap;
use z3::ast::{Ast, Int};
use z3::{Config, Context as Z3Context, SatResult, Solver};

/// Result of a verification attempt
#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub status: VerificationStatus,
    pub proof: Option<String>,
    pub confidence: f64,
}

/// Z3-based constraint verifier
pub struct Z3Verifier {
    timeout_ms: u64,
}

impl Z3Verifier {
    pub fn new(timeout_ms: u64) -> Self {
        Self { timeout_ms }
    }

    /// Verify constraints using Z3 SMT solver
    pub fn verify(
        &self,
        constraints: &[Constraint],
        hypothesis: &LlmHypothesis,
    ) -> Result<VerificationResult> {
        let cfg = Config::new();
        let ctx = Z3Context::new(&cfg);
        let solver = Solver::new(&ctx);

        // Set timeout
        let mut params = z3::Params::new(&ctx);
        params.set_u32("timeout", self.timeout_ms as u32);
        solver.set_params(&params);

        // Track variables
        let mut vars: HashMap<String, Int> = HashMap::new();

        // Convert constraints to Z3
        for constraint in constraints {
            match constraint {
                Constraint::VarDecl { name, value } => {
                    let var = Int::new_const(&ctx, name.as_str());
                    vars.insert(name.clone(), var.clone());

                    if let Some(val_expr) = value {
                        let val = self.expr_to_z3(&ctx, &vars, val_expr)?;
                        solver.assert(&var._eq(&val));
                    }
                }
                Constraint::BinOp { left, op, right } => {
                    let left_z3 = self.expr_to_z3(&ctx, &vars, left)?;
                    let right_z3 = self.expr_to_z3(&ctx, &vars, right)?;
                    let constraint_z3 = self.binop_to_z3(&left_z3, *op, &right_z3)?;
                    solver.assert(&constraint_z3);
                }
                Constraint::Assert(_expr) => {
                    // TODO: Assert should take Bool expressions, not Int expressions
                    // For MVP, we don't use this constraint type
                    // anyhow::bail!("Assert constraint not yet implemented for Int expressions")
                }
                Constraint::DivisionCheck {
                    numerator: _,
                    denominator,
                } => {
                    // Check if denominator can be zero
                    let den_z3 = self.expr_to_z3(&ctx, &vars, denominator)?;
                    let zero = Int::from_i64(&ctx, 0);

                    // Assert that denominator equals zero (we're checking if this is possible)
                    solver.assert(&den_z3._eq(&zero));
                }
            }
        }

        // Check satisfiability
        let result = solver.check();

        eprintln!("DEBUG Z3: result = {:?}", result);

        match result {
            SatResult::Sat => {
                // Bug is verified - denominator can be zero
                let model = solver.get_model().context("No model available")?;
                let proof = format!("Z3 found satisfying assignment: {}", model);

                Ok(VerificationResult {
                    status: VerificationStatus::Verified,
                    proof: Some(proof),
                    confidence: 0.95, // High confidence when Z3 proves it
                })
            }
            SatResult::Unsat => {
                // Bug is refuted - denominator cannot be zero
                let proof = "Z3 proved the condition is impossible (UNSAT)".to_string();

                Ok(VerificationResult {
                    status: VerificationStatus::Refuted,
                    proof: Some(proof),
                    confidence: 0.95,
                })
            }
            SatResult::Unknown => {
                // Z3 couldn't determine (timeout or too complex)
                Ok(VerificationResult {
                    status: VerificationStatus::Unknown,
                    proof: Some("Z3 timeout or unknown result".to_string()),
                    confidence: 0.5, // Low confidence when uncertain
                })
            }
        }
    }

    fn expr_to_z3<'ctx>(
        &self,
        ctx: &'ctx Z3Context,
        vars: &HashMap<String, Int<'ctx>>,
        expr: &ConstraintExpr,
    ) -> Result<Int<'ctx>> {
        match expr {
            ConstraintExpr::Variable(name) => vars
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Unknown variable: {}", name)),
            ConstraintExpr::Literal(val) => Ok(Int::from_i64(ctx, *val)),
            ConstraintExpr::BinOp { left, op, right } => {
                let left_z3 = self.expr_to_z3(ctx, vars, left)?;
                let right_z3 = self.expr_to_z3(ctx, vars, right)?;

                match op {
                    ConstraintOp::Add => Ok(left_z3 + right_z3),
                    ConstraintOp::Sub => Ok(left_z3 - right_z3),
                    ConstraintOp::Mul => Ok(left_z3 * right_z3),
                    ConstraintOp::Div => {
                        // Note: Z3 integer division
                        Ok(left_z3 / right_z3)
                    }
                    ConstraintOp::Mod => Ok(left_z3.modulo(&right_z3)),
                    _ => {
                        // For comparison ops in expressions, we can't directly return Int
                        // This is a simplified version - full implementation would need Bool conversion
                        anyhow::bail!("Comparison operators in expressions not yet supported")
                    }
                }
            }
        }
    }

    fn binop_to_z3<'ctx>(
        &self,
        left: &Int<'ctx>,
        op: ConstraintOp,
        right: &Int<'ctx>,
    ) -> Result<z3::ast::Bool<'ctx>> {
        match op {
            ConstraintOp::Eq => Ok(left._eq(right)),
            ConstraintOp::Ne => Ok(left._eq(right).not()),
            ConstraintOp::Lt => Ok(left.lt(right)),
            ConstraintOp::Le => Ok(left.le(right)),
            ConstraintOp::Gt => Ok(left.gt(right)),
            ConstraintOp::Ge => Ok(left.ge(right)),
            ConstraintOp::And | ConstraintOp::Or => {
                anyhow::bail!("Boolean operators require Bool type, not Int")
            }
            _ => anyhow::bail!("Arithmetic operators don't produce Bool constraints"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rustverify::{AnalysisType, Severity};

    #[test]
    fn test_verify_division_by_zero_possible() {
        let verifier = Z3Verifier::new(5000);

        // x / y where y is unconstrained (can be zero)
        let constraints = vec![
            Constraint::VarDecl {
                name: "x".to_string(),
                value: Some(ConstraintExpr::Literal(10)),
            },
            Constraint::VarDecl {
                name: "y".to_string(),
                value: None, // Unconstrained - can be any value including 0
            },
            Constraint::DivisionCheck {
                numerator: ConstraintExpr::Variable("x".to_string()),
                denominator: ConstraintExpr::Variable("y".to_string()),
            },
        ];

        let hypothesis = LlmHypothesis {
            analysis_type: AnalysisType::DivisionByZero,
            line: 3,
            column: 17,
            severity: Severity::High,
            description: "Division by zero possible".to_string(),
            recommendation: "Add check for y != 0".to_string(),
        };

        let result = verifier.verify(&constraints, &hypothesis).unwrap();
        assert_eq!(result.status, VerificationStatus::Verified);
        assert!(result.proof.is_some());
    }

    #[test]
    fn test_verify_division_by_zero_impossible() {
        let verifier = Z3Verifier::new(5000);

        // x / y where y > 0 (cannot be zero)
        let constraints = vec![
            Constraint::VarDecl {
                name: "x".to_string(),
                value: Some(ConstraintExpr::Literal(10)),
            },
            Constraint::VarDecl {
                name: "y".to_string(),
                value: None,
            },
            Constraint::BinOp {
                left: ConstraintExpr::Variable("y".to_string()),
                op: ConstraintOp::Gt,
                right: ConstraintExpr::Literal(0),
            },
            Constraint::DivisionCheck {
                numerator: ConstraintExpr::Variable("x".to_string()),
                denominator: ConstraintExpr::Variable("y".to_string()),
            },
        ];

        let hypothesis = LlmHypothesis {
            analysis_type: AnalysisType::DivisionByZero,
            line: 3,
            column: 17,
            severity: Severity::High,
            description: "Division by zero possible".to_string(),
            recommendation: "Add check for y != 0".to_string(),
        };

        let result = verifier.verify(&constraints, &hypothesis).unwrap();
        assert_eq!(result.status, VerificationStatus::Refuted);
    }

    #[test]
    fn test_literal_constraint() {
        let verifier = Z3Verifier::new(5000);

        // Division by literal zero: x / 0
        let constraints = vec![
            Constraint::VarDecl {
                name: "x".to_string(),
                value: Some(ConstraintExpr::Literal(10)),
            },
            Constraint::DivisionCheck {
                numerator: ConstraintExpr::Variable("x".to_string()),
                denominator: ConstraintExpr::Literal(0),
            },
        ];

        let hypothesis = LlmHypothesis {
            analysis_type: AnalysisType::DivisionByZero,
            line: 3,
            column: 17,
            severity: Severity::Critical,
            description: "Division by literal zero".to_string(),
            recommendation: "Remove division by zero".to_string(),
        };

        let result = verifier.verify(&constraints, &hypothesis).unwrap();
        assert_eq!(result.status, VerificationStatus::Verified);
    }
}
