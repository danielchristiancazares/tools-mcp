//! MCP WebFetch tool handler (application layer); delegates to [`crate::ports::WebFetcher`].

use crate::services::default_web_fetcher;
use crate::tool_outcome::ToolCallOutcome;
use crate::webfetch::FetchRequest;

/// MCP tool handler for WebFetch
pub async fn handle_webfetch(
    _id: Option<serde_json::Value>,
    args: serde_json::Value,
) -> ToolCallOutcome {
    let request = match ToolCallOutcome::parse_args::<FetchRequest>(args) {
        Ok(req) => req,
        Err(o) => return o,
    };

    let url = request.url.clone();
    let force_browser = request.force_browser;

    match default_web_fetcher().fetch(request).await {
        Ok(response) => {
            let json_text = serde_json::to_string(&response)
                .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {}\"}}", e));
            ToolCallOutcome::ok(serde_json::json!({
                "content": [{"type": "text", "text": json_text}],
                "isError": false
            }))
        }
        Err(e) => {
            let details_full = format!("{:#}", e);
            let (message, error_type, remediation) =
                classify_webfetch_error(&details_full, force_browser);

            ToolCallOutcome::err_with(
                message,
                [
                    ("error_type", serde_json::json!(error_type)),
                    ("url", serde_json::json!(url)),
                    ("force_browser", serde_json::json!(force_browser)),
                    (
                        "details",
                        serde_json::json!(truncate_tool_details(&details_full, 1200)),
                    ),
                    ("remediation", serde_json::json!(remediation)),
                ],
            )
        }
    }
}

fn truncate_tool_details(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return "…".to_string();
    }

    let mut char_count = 0usize;
    let mut truncation_byte_idx = input.len();

    for (byte_idx, _) in input.char_indices() {
        if char_count == max_chars {
            truncation_byte_idx = byte_idx;
            break;
        }
        char_count += 1;
    }

    if char_count == max_chars && truncation_byte_idx < input.len() {
        format!("{}…", &input[..truncation_byte_idx])
    } else {
        input.to_string()
    }
}

fn classify_webfetch_error(
    details: &str,
    force_browser: bool,
) -> (String, &'static str, Vec<String>) {
    let lower = details.to_ascii_lowercase();

    if lower.contains("ssrf validation failed")
        || lower.contains("cannot fetch from localhost")
        || lower.contains("private ip")
        || (lower.contains("scheme") && lower.contains("not allowed"))
        || lower.contains("refusing to fetch")
    {
        return (
            "WebFetch blocked this URL for safety (SSRF protection). Only public http/https URLs are allowed; localhost/private IPs and non-http schemes are rejected.".to_string(),
            "ssrf_blocked",
            vec![
                "Use a public http/https URL (not file://, localhost, or a private IP).".to_string(),
                "If you need local content, read files from disk with Read instead of WebFetch.".to_string(),
            ],
        );
    }

    if lower.contains("robots.txt") && (lower.contains("disallow") || lower.contains("disallowed"))
    {
        return (
            "WebFetch is blocked by robots.txt for this URL (the tool respects robots.txt).".to_string(),
            "robots_disallowed",
            vec![
                "Choose a different URL that is allowed by robots.txt, or use an official API/source.".to_string(),
                "If you already have the relevant text, paste/supply it directly instead of fetching.".to_string(),
            ],
        );
    }

    if lower.contains("chrome/chromium not installed") {
        let mut remediation = vec![
            "Install Chrome/Chromium on the host running the MCP server, then retry.".to_string(),
        ];
        if force_browser {
            remediation.push("Or set force_browser=false to attempt HTTP mode.".to_string());
        } else {
            remediation.push(
                "If the site requires JS rendering, browser rendering may be required; try a server-rendered docs page or install Chrome/Chromium.".to_string(),
            );
        }
        return (
            "WebFetch could not use browser rendering because Chrome/Chromium is not available."
                .to_string(),
            "browser_unavailable",
            remediation,
        );
    }

    if lower.contains("http 404") || lower.contains("resource not found") {
        return (
            "WebFetch got HTTP 404 (not found).".to_string(),
            "http_404",
            vec![
                "Double-check the URL for typos and try again.".to_string(),
                "If the site redirects from http->https or needs a trailing slash, try the canonical URL.".to_string(),
            ],
        );
    }

    if lower.contains("http error") {
        return (
            "WebFetch hit an HTTP error fetching the URL.".to_string(),
            "http_error",
            vec![
                "Retry later; the site may be temporarily unavailable.".to_string(),
                "If the site blocks bots, try an alternate source or provide the text directly."
                    .to_string(),
            ],
        );
    }

    if lower.contains("timed out") || lower.contains("timeout") {
        return (
            "WebFetch timed out while fetching/rendering the URL.".to_string(),
            "timeout",
            vec![
                "Retry the request; transient timeouts are common.".to_string(),
                "If the page is heavy, try a simpler URL (e.g., a print view or docs page) or enable caching (no_cache=false).".to_string(),
            ],
        );
    }

    if lower.contains("dns")
        || lower.contains("name or service")
        || lower.contains("failed to resolve")
    {
        return (
            "WebFetch failed due to a DNS/network error.".to_string(),
            "network",
            vec![
                "Check the URL hostname and network connectivity, then retry.".to_string(),
                "If this is an internal-only host, WebFetch will not be able to access it."
                    .to_string(),
            ],
        );
    }

    (
        "WebFetch failed.".to_string(),
        "unknown",
        vec![
            "Check the details field for the underlying error and retry.".to_string(),
            "Try another URL or provide the text directly if available.".to_string(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::truncate_tool_details;

    #[test]
    fn truncate_tool_details_truncates_ascii() {
        let out = truncate_tool_details("abcdef", 3);
        assert_eq!(out, "abc…");
    }

    #[test]
    fn truncate_tool_details_preserves_short_input() {
        let out = truncate_tool_details("abc", 10);
        assert_eq!(out, "abc");
    }

    #[test]
    fn truncate_tool_details_handles_unicode_boundaries() {
        let out = truncate_tool_details("éééé", 3);
        assert_eq!(out, "ééé…");
    }
}
