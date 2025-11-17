/// Known JS-heavy domains that require browser rendering
/// These sites typically use client-side rendering (React, Vue, Angular, etc.)
/// and return minimal HTML without JavaScript execution
use url::Url;

/// List of domain patterns that are known to require JavaScript rendering
const JS_HEAVY_DOMAINS: &[&str] = &[
    // Documentation sites with React/Next.js
    "react.dev",
    "nextjs.org",
    "vuejs.org",
    "angular.dev",
    "angular.io",
    "svelte.dev",
    // Content platforms
    "medium.com",
    "notion.so",
    "notion.site",
    // Developer tools/platforms
    "vercel.com",
    "netlify.app",
    "cloudflare.com",
    // Modern docs platforms
    "gitbook.io",
    "readme.io",
    "docusaurus.io",
    // Single-page applications
    "app.slack.com",
    "web.telegram.org",
    "discord.com/app",
    // Common SPA patterns
    "*.vercel.app",
    "*.netlify.app",
    "*.pages.dev",
    "*.web.app",
];

/// Check if a URL's domain matches the whitelist of known JS-heavy sites
pub fn is_whitelisted_js_heavy(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };

    let Some(host) = parsed.host_str() else {
        return false;
    };

    for pattern in JS_HEAVY_DOMAINS {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            // Wildcard pattern: *.vercel.app matches foo.vercel.app
            if host.ends_with(suffix) {
                return true;
            }
        } else if host == *pattern || host.ends_with(&format!(".{}", pattern)) {
            // Exact match or subdomain match
            // e.g., "medium.com" matches both "medium.com" and "blog.medium.com"
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
