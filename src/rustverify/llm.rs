// LLM integration for hypothesis generation using Claude API

use crate::rustverify::parser::ParsedRust;
use crate::rustverify::{AnalysisType, Severity};
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A hypothesis about a potential bug from the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmHypothesis {
    pub analysis_type: AnalysisType,
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub description: String,
    pub recommendation: String,
}

/// Claude API client for code analysis
pub struct LlmAnalyzer {
    api_key: String,
    client: Client,
}

impl LlmAnalyzer {
    pub fn new(api_key: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            api_key: api_key.to_string(),
            client,
        }
    }

    /// Analyze Rust code with Claude to generate hypotheses
    pub async fn analyze(
        &self,
        parsed: &ParsedRust,
        analysis_types: &[AnalysisType],
    ) -> Result<Vec<LlmHypothesis>> {
        let prompt = self.build_prompt(parsed, analysis_types);
        let response = self.call_claude(&prompt).await?;
        self.parse_response(&response)
    }

    fn build_prompt(&self, parsed: &ParsedRust, analysis_types: &[AnalysisType]) -> String {
        let types_str = analysis_types
            .iter()
            .map(|t| format!("{:?}", t))
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            r#"Analyze the following Rust code for potential bugs. Focus on: {types_str}

For each issue you find, provide:
1. The exact line and column number (1-indexed)
2. The type of issue (overflow, division-by-zero, logic, unsafe, panic)
3. Severity (critical, high, medium, low)
4. A clear description of the potential bug
5. A recommendation for fixing it

Return your analysis as a JSON array of findings. Each finding must have this structure:
{{
  "analysis_type": "division-by-zero" | "overflow" | "logic" | "unsafe" | "panic",
  "line": <number>,
  "column": <number>,
  "severity": "critical" | "high" | "medium" | "low",
  "description": "<detailed explanation>",
  "recommendation": "<how to fix>"
}}

Rust code to analyze:
```rust
{}
```

IMPORTANT:
- Be conservative: only report issues you're confident about
- Focus on actual bugs, not style issues
- For division/modulo, check if the denominator could be zero
- For panics, look for unwrap(), expect(), and unchecked indexing
- For overflow, look for arithmetic operations on integer types
- Only return the JSON array, no other text"#,
            parsed.source
        )
    }

    async fn call_claude(&self, prompt: &str) -> Result<String> {
        #[derive(Serialize)]
        struct ClaudeRequest {
            model: String,
            max_tokens: u32,
            messages: Vec<Message>,
        }

        #[derive(Serialize)]
        struct Message {
            role: String,
            content: String,
        }

        #[derive(Deserialize)]
        struct ClaudeResponse {
            content: Vec<ContentBlock>,
        }

        #[derive(Deserialize)]
        struct ContentBlock {
            text: String,
        }

        let request = ClaudeRequest {
            model: "claude-haiku-4-5-20251001".to_string(),
            max_tokens: 4000,
            messages: vec![Message {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to call Claude API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Claude API error {}: {}", status, body);
        }

        let claude_response: ClaudeResponse = response
            .json()
            .await
            .context("Failed to parse Claude response")?;

        Ok(claude_response
            .content
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default())
    }

    fn parse_response(&self, response: &str) -> Result<Vec<LlmHypothesis>> {
        // Extract JSON array from response (handle markdown code blocks)
        let json_str = if let Some(start) = response.find('[') {
            if let Some(end) = response.rfind(']') {
                &response[start..=end]
            } else {
                response
            }
        } else {
            response
        };

        let hypotheses: Vec<LlmHypothesis> =
            serde_json::from_str(json_str).context("Failed to parse LLM response as JSON")?;

        Ok(hypotheses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_response() {
        let analyzer = LlmAnalyzer::new("test-key");
        let response = r#"
        [
            {
                "analysis_type": "division-by-zero",
                "line": 3,
                "column": 17,
                "severity": "high",
                "description": "Variable y could be zero",
                "recommendation": "Add check: if y != 0"
            }
        ]
        "#;

        let hypotheses = analyzer.parse_response(response).unwrap();
        assert_eq!(hypotheses.len(), 1);
        assert_eq!(hypotheses[0].line, 3);
        assert_eq!(hypotheses[0].analysis_type, AnalysisType::DivisionByZero);
    }

    #[test]
    fn test_parse_response_with_markdown() {
        let analyzer = LlmAnalyzer::new("test-key");
        let response = r#"
        Here are the findings:
        ```json
        [
            {
                "analysis_type": "panic",
                "line": 5,
                "column": 20,
                "severity": "medium",
                "description": "unwrap() could panic",
                "recommendation": "Use pattern matching or ?"
            }
        ]
        ```
        "#;

        let hypotheses = analyzer.parse_response(response).unwrap();
        assert_eq!(hypotheses.len(), 1);
        assert_eq!(hypotheses[0].analysis_type, AnalysisType::Panic);
    }

    #[test]
    fn test_build_prompt() {
        let analyzer = LlmAnalyzer::new("test-key");
        let parsed = ParsedRust {
            source: "fn test() {}".to_string(),
            ast: syn::parse_file("fn test() {}").unwrap(),
            file_path: "test.rs".to_string(),
        };

        let prompt = analyzer.build_prompt(&parsed, &[AnalysisType::DivisionByZero]);
        assert!(prompt.contains("division-by-zero"));
        assert!(prompt.contains("fn test() {}"));
    }
}
