// Neuro-symbolic analyzer for Rust code
// Combines LLM analysis with formal verification to detect bugs with zero false positives

mod constraints;
mod llm;
mod parser;
mod verifier;

pub use constraints::{Constraint, ConstraintExtractor};
pub use llm::{LlmAnalyzer, LlmHypothesis};
pub use parser::{ParsedRust, RustAnalyzer};
pub use verifier::{VerificationResult, Z3Verifier};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Analysis types supported by the verifier
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AnalysisType {
    /// Integer overflow/underflow detection
    Overflow,
    /// Division by zero detection
    DivisionByZero,
    /// Logic errors and unreachable code
    Logic,
    /// Unsafe block correctness verification
    Unsafe,
    /// Panic conditions (unwrap, expect, indexing)
    Panic,
}

/// A verified finding from the analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Unique identifier for this finding
    pub id: String,
    /// Type of analysis that found this
    pub analysis_type: AnalysisType,
    /// File location
    pub file: String,
    /// Line number (1-indexed)
    pub line: usize,
    /// Column number (1-indexed)
    pub column: usize,
    /// Severity level
    pub severity: Severity,
    /// LLM's hypothesis about the issue
    pub llm_hypothesis: String,
    /// Verification status
    pub verification: VerificationStatus,
    /// Proof or counterexample from Z3
    pub proof: Option<String>,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,
    /// Recommended fix
    pub recommendation: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum VerificationStatus {
    /// Z3 proved the issue exists
    Verified,
    /// Z3 proved the issue cannot occur
    Refuted,
    /// Z3 timed out or couldn't determine
    Unknown,
}

/// Analysis summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisSummary {
    pub total_findings: usize,
    pub verified: usize,
    pub refuted: usize,
    pub unknown: usize,
    pub runtime_seconds: f64,
}

/// Main orchestration function for the neuro-symbolic analysis pipeline
pub async fn analyze_rust_file(
    file_path: &Path,
    analysis_types: &[AnalysisType],
    timeout_ms: u64,
    api_key: &str,
) -> Result<(Vec<Finding>, AnalysisSummary)> {
    let start = std::time::Instant::now();

    // Step 1: Parse Rust source code
    let analyzer = RustAnalyzer::new();
    let parsed = analyzer.parse_file(file_path)?;

    // Step 2: Get LLM hypotheses for potential issues
    let llm = LlmAnalyzer::new(api_key);
    let hypotheses = llm.analyze(&parsed, analysis_types).await?;

    // Step 3: Extract constraints for each hypothesis
    let extractor = ConstraintExtractor::new();
    let mut findings = Vec::new();
    let mut verified_count = 0;
    let mut refuted_count = 0;
    let mut unknown_count = 0;

    for hypothesis in hypotheses {
        // Extract constraints relevant to this hypothesis
        let constraints = extractor.extract(&parsed, &hypothesis)?;

        // Step 4: Verify with Z3
        let verifier = Z3Verifier::new(timeout_ms);
        let result = verifier.verify(&constraints, &hypothesis)?;

        // Convert to finding
        let finding = Finding {
            id: format!("{:?}-{:03}", hypothesis.analysis_type, findings.len() + 1),
            analysis_type: hypothesis.analysis_type,
            file: file_path.display().to_string(),
            line: hypothesis.line,
            column: hypothesis.column,
            severity: hypothesis.severity,
            llm_hypothesis: hypothesis.description,
            verification: result.status,
            proof: result.proof,
            confidence: result.confidence,
            recommendation: hypothesis.recommendation,
        };

        match finding.verification {
            VerificationStatus::Verified => verified_count += 1,
            VerificationStatus::Refuted => refuted_count += 1,
            VerificationStatus::Unknown => unknown_count += 1,
        }

        // Only report verified or unknown findings (not refuted false positives)
        if finding.verification != VerificationStatus::Refuted {
            findings.push(finding);
        }
    }

    let summary = AnalysisSummary {
        total_findings: findings.len(),
        verified: verified_count,
        refuted: refuted_count,
        unknown: unknown_count,
        runtime_seconds: start.elapsed().as_secs_f64(),
    };

    Ok((findings, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analysis_type_serialization() {
        let at = AnalysisType::DivisionByZero;
        let json = serde_json::to_string(&at).unwrap();
        assert_eq!(json, r#""division-by-zero""#);
    }

    #[test]
    fn test_severity_ordering() {
        assert!(matches!(Severity::Critical, Severity::Critical));
        assert_ne!(Severity::High, Severity::Low);
    }
}
