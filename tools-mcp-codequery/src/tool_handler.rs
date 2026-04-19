//! `CodeQuery` MCP tool handler.
//!
//! Validates input, resolves the target vector store (see [`store_resolution`]), discovers
//! files when none are provided (see [`discovery`]), then delegates the index + query to
//! the [`crate::adapters::outbound::FileSearchCoreEngine`]. Surfaces structured remediation
//! hints on common failure modes (auth, rate limit, timeout, network).
//!
//! [`store_resolution`]: crate::store_resolution
//! [`discovery`]: crate::discovery

use serde::Deserialize;
use serde_json::Value;

use crate::adapters::outbound::FileSearchCoreEngine;
use crate::discovery::{default_workspace_scope, discover_default_file_paths};
use crate::ports::CodeQueryEngine;
use crate::store_resolution::resolve_vector_store_id;
use tools_mcp_core::{ToolCallOutcome, validation};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeQueryRequest {
    #[serde(default)]
    vector_store_id: Option<String>,
    #[serde(default)]
    vector_store_name: Option<String>,
    #[serde(default)]
    query: String,
    #[serde(default)]
    file_paths: Vec<String>,
    #[serde(default)]
    concurrent_limit: Option<u64>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    max_num_results: Option<u64>,
    #[serde(default)]
    include_results: Option<bool>,
}

pub async fn handle_code_query(_id: Option<Value>, args: Value) -> ToolCallOutcome {
    let req = match ToolCallOutcome::parse_args::<CodeQueryRequest>(&args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return ToolCallOutcome::err_with(
            "OPENAI_API_KEY is not set. CodeQuery uses the OpenAI API (vector stores) and requires an API key.",
            [
                ("error_type", serde_json::json!("missing_env")),
                ("env_var", serde_json::json!("OPENAI_API_KEY")),
                (
                    "remediation",
                    serde_json::json!([
                        "Set OPENAI_API_KEY in the environment before starting the MCP server, then retry CodeQuery.",
                        "If you cannot provide an API key, use Search/Read/Glob for local-only code navigation.",
                    ]),
                ),
            ],
        );
    }

    let vector_store_id_arg = req
        .vector_store_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string);
    let explicit_vector_store_name = req
        .vector_store_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string);
    let mut vector_store_name = explicit_vector_store_name.clone();
    let query = req.query.as_str();

    if let Err(o) = validation::validate_non_empty(query, "query", None) {
        return o;
    }

    let default_workspace_scope = if vector_store_id_arg.is_none() && vector_store_name.is_none() {
        // Default the store name to the repository directory so every checkout gets a stable
        // vector store without extra MCP arguments. Advanced callers can override via
        // `vector_store_name` when needed.
        let workspace_scope = match default_workspace_scope() {
            Ok(scope) => scope,
            Err(err) => {
                return ToolCallOutcome::err(format!(
                    "CodeQuery could not infer a vector store name: {err}. Provide vector_store_name explicitly."
                ));
            }
        };
        vector_store_name = Some(workspace_scope.default_store_name.clone());
        if vector_store_name.is_none() {
            return ToolCallOutcome::err(
                "CodeQuery could not infer a vector store name. Provide vector_store_name explicitly.",
            );
        }
        Some(workspace_scope)
    } else {
        None
    };

    let mut file_paths = req.file_paths;

    if file_paths.is_empty() {
        match discover_default_file_paths(
            default_workspace_scope
                .as_ref()
                .map(|scope| scope.root.as_path()),
        ) {
            Ok(mut discovered) => {
                tracing::info!(
                    "CodeQuery auto-discovered {} file(s) for indexing",
                    discovered.len()
                );
                file_paths.append(&mut discovered);
            }
            Err(err) => {
                let message = format!(
                    "CodeQuery could not discover local files: {err}. Remediation: run the server from the repo root or pass file_paths explicitly."
                );
                tracing::error!(error = %message);
                return ToolCallOutcome::err(message);
            }
        }
    }

    let concurrent_limit = req.concurrent_limit.unwrap_or(5) as usize;
    if !(1..=20).contains(&concurrent_limit) {
        return ToolCallOutcome::err(format!(
            "concurrent_limit must be between 1 and 20 (got {concurrent_limit}). Use a smaller value to reduce API concurrency."
        ));
    }

    let timeout_ms = req.timeout_ms.unwrap_or(60_000);
    if timeout_ms < 1_000 {
        return ToolCallOutcome::err(format!(
            "timeout_ms must be at least 1000 milliseconds (got {timeout_ms}). Increase timeout_ms for large repos or slow networks."
        ));
    }

    let include_results = req.include_results.unwrap_or(false);
    let max_num_results = req.max_num_results.map(|n| n as u32);
    let model_override = req.model;

    let client = reqwest::Client::new();
    let cfg = openai_file_search_core::ApiConfig::new(
        api_key,
        model_override.as_deref().unwrap_or("gpt-4o"),
    );

    let vector_store_id = if let Some(id) = vector_store_id_arg {
        id
    } else {
        let Some(name) = vector_store_name.as_deref() else {
            return ToolCallOutcome::err("CodeQuery could not determine a vector store name.");
        };

        let cache_lookup_key = default_workspace_scope
            .as_ref()
            .map_or(name, |scope| scope.cache_key.as_str());

        match resolve_vector_store_id(&client, &cfg, cache_lookup_key, name).await {
            Ok(id) => id,
            Err(e) => {
                return ToolCallOutcome::err(format!(
                    "failed to resolve vector store name '{name}': {e}"
                ));
            }
        }
    };

    let engine = FileSearchCoreEngine;
    match engine
        .execute(
            &client,
            &cfg,
            &vector_store_id,
            &file_paths,
            query,
            openai_file_search_core::CodeQueryOptions {
                concurrent_limit,
                timeout_ms,
                model: model_override.as_deref(),
                max_num_results,
                include_results,
            },
        )
        .await
    {
        Ok((text, reindex_summary)) => {
            let mut content = vec![serde_json::json!({
                "type": "text",
                "text": text
            })];

            if let Some(summary) = reindex_summary {
                let summary_text =
                    serde_json::to_string(&summary).unwrap_or_else(|_| summary.to_string());
                content.push(serde_json::json!({
                    "type": "text",
                    "text": summary_text
                }));
            }

            ToolCallOutcome::ok(serde_json::json!({
                "content": content,
                "isError": false
            }))
        }
        Err(e) => {
            let error_message = e.to_string();
            let lower = error_message.to_ascii_lowercase();

            // Avoid dumping huge server responses into the primary message; keep a bounded
            // `details` field for debugging while still giving the model actionable hints.
            const MAX_DETAILS_CHARS: usize = 1200;
            let details = truncate_error_details(&error_message, MAX_DETAILS_CHARS);

            let mut remediation: Vec<String> = Vec::new();
            if lower.contains("http 401")
                || lower.contains("unauthorized")
                || lower.contains("invalid api key")
            {
                remediation.push(
                    "Authentication failed. Verify OPENAI_API_KEY is valid, then restart the MCP server and retry."
                        .to_string(),
                );
            }
            if lower.contains("http 429") || lower.contains("rate limit") {
                remediation.push(
                    "You may be rate-limited. Retry later and/or reduce concurrent_limit (e.g., 1-3)."
                        .to_string(),
                );
            }
            if lower.contains("timeout") {
                remediation.push(
                    "Indexing/search timed out. Increase timeout_ms (especially for large repos) and retry."
                        .to_string(),
                );
            }
            if lower.contains("dns") || lower.contains("connection") || lower.contains("network") {
                remediation.push("Network/DNS error. Check connectivity and retry.".to_string());
            }
            if remediation.is_empty() {
                remediation.push(
                    "Retry CodeQuery; transient OpenAI/network errors often resolve.".to_string(),
                );
                remediation.push(
                    "If the repo is large, pass file_paths to limit indexing scope and reduce work."
                        .to_string(),
                );
            }
            remediation
                .push("Fallback: use Search/Read/Glob for local-only code navigation.".to_string());

            let headline = if lower.contains("code_query reindex failed") {
                "CodeQuery indexing failed after multiple attempts."
            } else {
                "CodeQuery failed."
            };

            tracing::error!("CodeQuery error: {}", error_message);

            ToolCallOutcome::err_with(
                headline,
                [
                    ("error_type", serde_json::json!("codequery_failure")),
                    ("details", serde_json::json!(details)),
                    ("remediation", serde_json::json!(remediation)),
                ],
            )
        }
    }
}

/// Truncates `s` to `max_chars` characters at a UTF-8 boundary, appending `…` on truncation.
fn truncate_error_details(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_error_details_handles_utf8_boundary() {
        // Build a string where a 3-byte UTF-8 character (€) starts at byte 1199.
        // The old buggy code would slice at byte 1200 and panic.
        let prefix = "a".repeat(1198);
        let input = format!("{prefix}€tail"); // € spans bytes 1198-1200
        assert!(input.len() > 1200);

        let result = truncate_error_details(&input, 1200);
        assert!(result.chars().count() == 1201); // 1200 chars + ellipsis
        assert!(result.ends_with('…'));
        assert!(result.contains('€'));
    }

    #[test]
    fn truncate_error_details_returns_unchanged_when_short() {
        let input = "short error";
        let result = truncate_error_details(input, 1200);
        assert_eq!(result, input);
    }

    #[test]
    fn truncate_error_details_handles_multibyte_chars() {
        let input = "🎉🎊🎈🎁🎀";
        assert_eq!(input.len(), 20);
        assert_eq!(input.chars().count(), 5);

        let result = truncate_error_details(input, 3);
        assert_eq!(result.chars().count(), 4); // 3 chars + ellipsis
        assert_eq!(result, "🎉🎊🎈…");
    }
}
