//! Whitelist of known JavaScript-heavy domains requiring browser rendering.
//!
//! This module maintains a curated list of domains known to require JavaScript
//! execution to render meaningful content. URLs matching these domains bypass
//! HTTP-first fetching and go directly to browser rendering.
//!
//! ## Why a Whitelist?
//!
//! While heuristic detection (see [`super::heuristics`]) can identify JS-heavy
//! sites, it requires fetching the page first. For known SPA domains, this
//! wastes a round-trip. The whitelist provides instant classification.
//!
//! ## Pattern Types
//!
//! The whitelist supports two pattern types:
//!
//! 1. **Exact/subdomain match**: `"example.com"` matches both `example.com`
//!    and any subdomain like `www.example.com` or `blog.example.com`
//!
//! 2. **Wildcard match**: `"*.example.com"` matches only subdomains like
//!    `app.example.com`, NOT `example.com` itself
//!
//! ## Adding New Domains
//!
//! Add domains to `JS_HEAVY_DOMAINS` when:
//! - The domain consistently serves SPA content
//! - HTTP fetch returns empty/minimal HTML
//! - Heuristic detection would always trigger browser fallback

use url::Url;

/// Curated list of domains known to require JavaScript rendering.
///
/// Organized by category for maintainability. Patterns can be:
/// - Exact domain: `"example.com"` (matches example.com and subdomains)
/// - Wildcard: `"*.example.com"` (matches only subdomains, not root)
const JS_HEAVY_DOMAINS: &[&str] = &[
    // ========================================================================
    // Framework Documentation Sites (React/Next.js/Vue/Angular)
    // ========================================================================
    "react.dev",
    "nextjs.org",
    "vuejs.org",
    "angular.dev",
    "angular.io",
    "svelte.dev",
    // ========================================================================
    // Content Platforms (CSR-heavy)
    // ========================================================================
    "medium.com",
    "notion.so",
    "notion.site",
    // ========================================================================
    // Developer Platforms
    // ========================================================================
    "vercel.com",
    "netlify.app",
    "cloudflare.com",
    // ========================================================================
    // Documentation Platforms
    // ========================================================================
    "gitbook.io",
    "readme.io",
    "docusaurus.io",
    // ========================================================================
    // Single-Page Applications
    // ========================================================================
    "app.slack.com",
    "web.telegram.org",
    "discord.com",
    // ========================================================================
    // Wildcard Patterns (hosting platforms serving user SPAs)
    // ========================================================================
    "*.vercel.app",
    "*.netlify.app",
    "*.pages.dev",
    "*.web.app",
];

// ============================================================================
// Pattern Matching
// ============================================================================

/// Checks if a URL's domain is in the JS-heavy whitelist.
///
/// This is the main entry point for whitelist checking. URLs matching
/// whitelisted domains will use browser rendering immediately without
/// attempting HTTP-first fetching.
///
/// # Pattern Matching Rules
///
/// 1. **Exact match**: `"example.com"` in whitelist
///    - Matches: `example.com`
///    - Matches: `www.example.com` (subdomain)
///    - Matches: `blog.example.com` (subdomain)
///
/// 2. **Wildcard match**: `"*.example.com"` in whitelist
///    - Matches: `app.example.com`
///    - Matches: `www.example.com`
///    - Does NOT match: `example.com` (root domain)
///
/// # Arguments
///
/// * `url` - The URL to check (must be parseable)
///
/// # Returns
///
/// `true` if the URL's domain matches any whitelist pattern, `false` otherwise.
/// Returns `false` for unparseable URLs.
///
/// # Examples
///
/// ```ignore
/// assert!(is_whitelisted_js_heavy("https://medium.com/article"));
/// assert!(is_whitelisted_js_heavy("https://myapp.vercel.app/"));
/// assert!(!is_whitelisted_js_heavy("https://example.com/"));
/// ```
pub fn is_whitelisted_js_heavy(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };

    let Some(host) = parsed.host_str() else {
        return false;
    };

    for pattern in JS_HEAVY_DOMAINS {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            // Wildcard: *.vercel.app matches foo.vercel.app but NOT vercel.app
            if host != suffix && host.ends_with(&format!(".{}", suffix)) {
                return true;
            }
        } else if host == *pattern || host.ends_with(&format!(".{}", pattern)) {
            // Exact or subdomain: medium.com matches medium.com AND blog.medium.com
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        assert!(is_whitelisted_js_heavy("https://medium.com/article"));
        assert!(is_whitelisted_js_heavy("https://notion.so/page"));
        assert!(is_whitelisted_js_heavy("https://react.dev/docs"));
    }

    #[test]
    fn test_subdomain_match() {
        assert!(is_whitelisted_js_heavy("https://blog.medium.com/article"));
        assert!(is_whitelisted_js_heavy("https://www.notion.so/page"));
    }

    #[test]
    fn test_wildcard_match() {
        assert!(is_whitelisted_js_heavy("https://myapp.vercel.app/"));
        assert!(is_whitelisted_js_heavy("https://test.netlify.app/docs"));
        assert!(is_whitelisted_js_heavy("https://project.pages.dev/"));
    }

    #[test]
    fn test_no_match() {
        assert!(!is_whitelisted_js_heavy("https://example.com/"));
        assert!(!is_whitelisted_js_heavy("https://github.com/repo"));
        assert!(!is_whitelisted_js_heavy(
            "https://stackoverflow.com/questions"
        ));
    }

    #[test]
    fn test_invalid_url() {
        assert!(!is_whitelisted_js_heavy("not a url"));
        assert!(!is_whitelisted_js_heavy(""));
    }
}
