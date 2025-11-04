/// Heuristics for detecting JavaScript-heavy websites that require browser rendering
/// These indicators help identify client-side rendered (CSR) applications
use scraper::{Html, Selector};

/// Threshold for considering content as "empty" (in characters)
const MIN_CONTENT_CHARS: usize = 500;

/// Threshold for script tag density (number of script tags)
const MAX_SCRIPT_TAGS: usize = 5;

/// Minimum content-to-HTML ratio to avoid browser rendering
const MIN_CONTENT_RATIO: f64 = 0.1;

/// Container for heuristic analysis results
#[derive(Debug, Clone)]
pub struct JsHeuristicResult {
    pub is_js_heavy: bool,
    pub confidence: f64,  // 0.0 to 1.0
    pub reasons: Vec<String>,
}

impl JsHeuristicResult {
    fn new() -> Self {
        Self {
            is_js_heavy: false,
            confidence: 0.0,
            reasons: Vec::new(),
        }
    }

    fn add_indicator(&mut self, weight: f64, reason: String) {
        self.confidence += weight;
        self.reasons.push(reason);
    }

    fn finalize(&mut self, threshold: f64) {
        self.is_js_heavy = self.confidence >= threshold;
        self.confidence = self.confidence.min(1.0);
    }
}

/// Analyze HTML and extracted content to determine if site is JS-heavy
/// Returns true if heuristics suggest browser rendering is needed
pub fn analyze_js_heavy(
    html_body: &str,
    extracted_markdown: &str,
    content_type: Option<&str>,
    content_length: Option<usize>,
) -> JsHeuristicResult {
    let mut result = JsHeuristicResult::new();

    // Skip analysis for non-HTML content
    if let Some(ct) = content_type {
        if !ct.contains("text/html") {
            return result;
        }
    }

    // Heuristic 1: Empty or minimal body with SPA root divs
    if check_empty_spa_shell(html_body, extracted_markdown, &mut result) {
        result.add_indicator(0.5, "Empty SPA shell detected".to_string());
    }

    // Heuristic 2: High script tag density
    if check_script_density(html_body, &mut result) {
        result.add_indicator(0.25, "High script tag density".to_string());
    }

    // Heuristic 3: Framework signatures in HTML
    if check_framework_signatures(html_body, &mut result) {
        result.add_indicator(0.3, "Framework signatures detected".to_string());
    }

    // Heuristic 4: Content-Type and size hints
    if check_header_hints(content_length, &mut result) {
        result.add_indicator(0.15, "Small HTML payload detected".to_string());
    }

    // Heuristic 5: Explicit JavaScript requirement warnings
    if check_noscript_warnings(html_body, &mut result) {
        result.add_indicator(0.5, "Explicit JS requirement detected".to_string());
    }

    // Threshold: 0.5 = 50% confidence
    result.finalize(0.5);

    result
}

/// Heuristic 1: Check for empty body with SPA mount point divs
fn check_empty_spa_shell(html_body: &str, extracted_markdown: &str, result: &mut JsHeuristicResult) -> bool {
    // Check if extracted content is minimal
    let content_is_minimal = extracted_markdown.trim().len() < MIN_CONTENT_CHARS;

    if !content_is_minimal {
        return false;
    }

    // Check for common SPA root element patterns
    let spa_patterns = [
        r#"<div id="root""#,
        r#"<div id="app""#,
        r#"<div id="__next""#,
        r#"<div id="root-container""#,
        r#"<div id="app-root""#,
        r#"<div class="app""#,
        r#"<div data-reactroot"#,
    ];

    for pattern in &spa_patterns {
        if html_body.contains(pattern) {
            return true;
        }
    }

    // Check content-to-HTML ratio
    let html_len = html_body.len() as f64;
    let content_len = extracted_markdown.len() as f64;

    if html_len > 0.0 {
        let ratio = content_len / html_len;
        if ratio < MIN_CONTENT_RATIO {
            result.add_indicator(
                0.2,
                format!("Low content ratio: {:.2}%", ratio * 100.0)
            );
            return true;
        }
    }

    false
}

/// Heuristic 2: Check for high script tag density or bundle patterns
fn check_script_density(html_body: &str, _result: &mut JsHeuristicResult) -> bool {
    let html = Html::parse_document(html_body);

    // Count script tags
    if let Ok(script_selector) = Selector::parse("script[src]") {
        let script_count = html.select(&script_selector).count();

        if script_count > MAX_SCRIPT_TAGS {
            return true;
        }
    }

    // Check for common bundle naming patterns
    let bundle_patterns = [
        "chunk.js",
        "bundle.js",
        "vendor.js",
        "app.js",
        "main.js",
        ".chunk.",
        ".bundle.",
    ];

    for pattern in &bundle_patterns {
        if html_body.matches(pattern).count() >= 2 {
            return true;
        }
    }

    false
}

/// Heuristic 3: Check for framework-specific attributes and patterns
fn check_framework_signatures(html_body: &str, result: &mut JsHeuristicResult) -> bool {
    let mut found_frameworks = Vec::new();

    // React signatures
    let react_patterns = ["data-reactroot", "data-reactid", "__reactContainer", "__REACT"];
    if react_patterns.iter().any(|p| html_body.contains(p)) {
        found_frameworks.push("React");
    }

    // Vue signatures
    let vue_patterns = ["data-v-", "v-cloak", "__vue", "__VUE__"];
    if vue_patterns.iter().any(|p| html_body.contains(p)) {
        found_frameworks.push("Vue");
    }

    // Angular signatures
    let angular_patterns = ["ng-app", "ng-version", "ng-binding", "[ng-"];
    if angular_patterns.iter().any(|p| html_body.contains(p)) {
        found_frameworks.push("Angular");
    }

    // Next.js signatures
    if html_body.contains("__NEXT_DATA__") || html_body.contains("_next/static") {
        found_frameworks.push("Next.js");
    }

    // Svelte signatures
    if html_body.contains("svelte-") || html_body.contains("__SVELTE__") {
        found_frameworks.push("Svelte");
    }

    if !found_frameworks.is_empty() {
        result.add_indicator(
            0.1,
            format!("Framework detected: {}", found_frameworks.join(", "))
        );
        true
    } else {
        false
    }
}

/// Heuristic 4: Check HTTP headers and content size hints
fn check_header_hints(content_length: Option<usize>, _result: &mut JsHeuristicResult) -> bool {
    // Small HTML payload (< 5KB) often indicates a shell page
    if let Some(length) = content_length {
        if length < 5000 {
            return true;
        }
    }

    false
}

/// Heuristic 5: Check for explicit noscript warnings
fn check_noscript_warnings(html_body: &str, _result: &mut JsHeuristicResult) -> bool {
    let html = Html::parse_document(html_body);

    if let Ok(noscript_selector) = Selector::parse("noscript") {
        for element in html.select(&noscript_selector) {
            let text = element.text().collect::<String>().to_lowercase();

            // Look for common JavaScript requirement phrases
            let warning_phrases = [
                "enable javascript",
                "requires javascript",
                "javascript is required",
                "turn on javascript",
                "javascript disabled",
                "needs javascript",
            ];

            for phrase in &warning_phrases {
                if text.contains(phrase) {
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_spa_shell() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head><title>App</title></head>
            <body>
                <div id="root"></div>
                <script src="bundle.js"></script>
            </body>
            </html>
        "#;
        let markdown = "";  // Empty extracted content

        let result = analyze_js_heavy(html, markdown, Some("text/html"), None);
        assert!(result.is_js_heavy, "Should detect empty SPA shell");
        assert!(result.reasons.iter().any(|r| r.contains("Empty SPA")));
    }

    #[test]
    fn test_high_script_density() {
        let html = r#"
            <body>
                <script src="vendor.js"></script>
                <script src="main.chunk.js"></script>
                <script src="runtime.bundle.js"></script>
                <script src="app.chunk.js"></script>
                <script src="polyfills.js"></script>
                <script src="utils.js"></script>
            </body>
        "#;
        let markdown = "Some content";

        let result = analyze_js_heavy(html, markdown, Some("text/html"), None);
        assert!(result.confidence > 0.0, "Should detect high script density");
    }

    #[test]
    fn test_react_signatures() {
        let html = r#"
            <div data-reactroot="">
                <div data-reactid="1">Content</div>
            </div>
        "#;
        let markdown = "Content";

        let result = analyze_js_heavy(html, markdown, Some("text/html"), None);
        assert!(result.reasons.iter().any(|r| r.contains("React")));
    }

    #[test]
    fn test_noscript_warning() {
        let html = r#"
            <body>
                <noscript>You need to enable JavaScript to run this app.</noscript>
                <div id="root"></div>
            </body>
        "#;
        let markdown = "";

        let result = analyze_js_heavy(html, markdown, Some("text/html"), None);
        assert!(result.is_js_heavy, "Should detect noscript warning");
        assert!(result.reasons.iter().any(|r| r.contains("JS requirement")));
    }

    #[test]
    fn test_normal_html_page() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head><title>Blog Post</title></head>
            <body>
                <h1>Welcome to my blog</h1>
                <p>This is a long article with lots of content...</p>
                <p>More content here, making this a substantial page.</p>
                <p>Even more content to ensure we exceed the minimum threshold.</p>
                <p>Additional paragraphs to make this a realistic HTML page.</p>
                <p>The content continues with more text and information.</p>
                <p>This ensures the extracted markdown will be long enough.</p>
                <p>We want to avoid false positives for normal static pages.</p>
                <p>So we add plenty of content here to test the heuristics properly.</p>
            </body>
            </html>
        "#;
        let markdown = "Welcome to my blog\nThis is a long article with lots of content...\nMore content here, making this a substantial page.\nEven more content to ensure we exceed the minimum threshold.\nAdditional paragraphs to make this a realistic HTML page.\nThe content continues with more text and information.\nThis ensures the extracted markdown will be long enough.\nWe want to avoid false positives for normal static pages.\nSo we add plenty of content here to test the heuristics properly.";

        let result = analyze_js_heavy(html, markdown, Some("text/html"), None);
        assert!(!result.is_js_heavy, "Should not detect normal HTML as JS-heavy");
    }

    #[test]
    fn test_small_content_length() {
        let html = "<html><body><div id=\"root\"></div></body></html>";
        let markdown = "";

        let result = analyze_js_heavy(html, markdown, Some("text/html"), Some(2000));
        assert!(result.confidence > 0.0, "Should consider small content length");
    }
}
