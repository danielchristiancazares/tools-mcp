//! HTTP fetching with SSRF protection and robots.txt compliance.
//!
//! This module provides a high-level HTTP client that implements several layers
//! of protection against Server-Side Request Forgery (SSRF) attacks and
//! ensures compliance with `robots.txt` exclusion rules.
//!
//! ### SSRF Protection
//!
//! The module implements comprehensive SSRF validation:
//!
//! 1. **Scheme validation**: Only `http` and `https` schemes are allowed.
//! 2. **Hostname validation**: Localhost hostnames are blocked.
//! 3. **IP literal validation**: Direct requests to private or reserved IP ranges
//!    (RFC 1918, loopback, link-local, multicast, etc.) are blocked.
//! 4. **DNS validation**: Hostnames are resolved, and ALL returned IP addresses
//!    are checked against private/reserved ranges.
//! 5. **DNS Rebinding Mitigation**: For hostname-based URLs, the resolved IP is
//!    pinned for the duration of the request.
//!
//! ### Redirect Handling
//!
//! Redirects are followed manually (not by reqwest) to ensure SSRF validation
//! is performed on each hop. Maximum 5 redirects are allowed.
//!
//! ## robots.txt Compliance
//!
//! The module respects robots.txt directives:
//!
//! 1. Fetches and caches robots.txt per domain (up to 1024 domains)
//! 2. Uses `tools-webfetch/0.1` as the user agent for matching
//! 3. Rejects requests to disallowed paths with an error
//! 4. Missing robots.txt = allow all (per specification)
//!
//! ## Usage
//!
//! ```ignore
//! use crate::webfetch::http::{fetch_document, validate_url_ssrf};
//!
//! // Validate URL before any operation
//! validate_url_ssrf("https://example.com/page").await?;
//!
//! // Fetch document with full protection
//! let request = FetchRequest { url: "https://example.com".into(), ..Default::default() };
//! let body = fetch_document(&request).await?;
//! ```

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode, header};
use robotstxt::DefaultMatcher;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::sync::RwLock;
use url::Host;
use url::Url;

use crate::webfetch::types::FetchRequest;

// ============================================================================
// Configuration Constants
// ============================================================================

/// User agent string for HTTP requests and robots.txt matching.
const USER_AGENT: &str = "tools-webfetch/0.1";

/// Default timeout for HTTP requests (covers connect + response).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Maximum redirect hops before failing (prevents redirect loops).
const MAX_REDIRECTS: usize = 5;

/// Maximum cached robots.txt entries before eviction.
/// Uses simple clear-on-full strategy to bound memory usage.
const ROBOTS_CACHE_MAX_ENTRIES: usize = 1024;

/// Maximum allowed size for a fetched web document (25 MiB).
///
/// This limit is applied during HTTP fetching to prevent memory exhaustion
/// from oversized responses. It is intentionally larger than the MCP message
/// limit to allow for content extraction and chunking before final delivery.
pub const MAX_RESPONSE_BYTES: usize = 25 * 1024 * 1024;

// ============================================================================
// Global State
// ============================================================================

/// In-memory cache for robots.txt content, keyed by domain origin.
///
/// Uses `RwLock` for concurrent read access with exclusive write access.
/// Cache entries are `Option<String>` where `None` means "no robots.txt found"
/// (which is cached to avoid repeated 404 requests).
static ROBOTS_CACHE: RwLock<Option<HashMap<String, Option<String>>>> = RwLock::const_new(None);

// ============================================================================
// Types
// ============================================================================

/// Raw HTTP response payload before content extraction.
///
/// This intermediate type carries the raw bytes and metadata from the HTTP
/// layer to the extraction layer.
pub struct FetchedBody {
    /// Raw response body bytes.
    pub body: Vec<u8>,

    /// Content-Type header value, if present.
    /// Used to determine extraction strategy (HTML vs plain text).
    pub content_type: Option<String>,

    /// Timestamp when the content was fetched.
    /// Stored in cache and returned in response for freshness tracking.
    pub fetched_at: DateTime<Utc>,
}

// ============================================================================
// SSRF Protection
// ============================================================================

/// Checks if an IP address is private, reserved, or otherwise unsuitable for fetching.
///
/// This is the core SSRF protection check. Returns `true` for any address that
/// should not be accessed by a web fetcher.
///
/// ## Blocked IPv4 Ranges
///
/// | Range | Description |
/// |-------|-------------|
/// | `10.0.0.0/8` | Private (RFC 1918) |
/// | `172.16.0.0/12` | Private (RFC 1918) |
/// | `192.168.0.0/16` | Private (RFC 1918) |
/// | `127.0.0.0/8` | Loopback |
/// | `169.254.0.0/16` | Link-local |
/// | `0.0.0.0/8` | "This network" |
/// | `100.64.0.0/10` | Carrier-grade NAT |
/// | `255.255.255.255` | Broadcast |
/// | `192.0.2.0/24`, etc. | Documentation |
///
/// ## Blocked IPv6 Ranges
///
/// | Range | Description |
/// |-------|-------------|
/// | `::1` | Loopback |
/// | `::` | Unspecified |
/// | `fc00::/7` | Unique local (ULA) |
/// | `fe80::/10` | Link-local |
/// | `::ffff:x.x.x.x` | IPv4-mapped (checked as IPv4) |
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            ipv4.is_private()           // 10/8, 172.16/12, 192.168/16
                || ipv4.is_loopback()   // 127/8
                || ipv4.is_link_local() // 169.254/16
                || ipv4.is_broadcast()  // 255.255.255.255
                || ipv4.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
                || ipv4.is_unspecified()   // 0.0.0.0
                || ipv4.is_multicast()     // 224.0.0.0/4
                || ipv4.octets()[0] == 0   // 0/8 "This network"
                || ipv4.octets()[0] == 100 && (ipv4.octets()[1] & 0xC0 == 0x40) // 100.64/10 CGNAT
                || ipv4.octets()[0] >= 240 // 240.0.0.0/4 Reserved/Experimental
        }
        IpAddr::V6(ipv6) => {
            // IPv4-mapped addresses (::ffff:x.x.x.x) must be checked as IPv4
            if let Some(v4) = ipv6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            ipv6.is_loopback()      // ::1
                || ipv6.is_unspecified() // ::
                || ipv6.is_multicast()   // ff00::/8
                || ((ipv6.segments()[0] & 0xfe00) == 0xfc00) // fc00::/7 unique local
                || ((ipv6.segments()[0] & 0xffc0) == 0xfe80) // fe80::/10 link-local
        }
    }
}

/// Validates a URL for SSRF protection.
///
/// This is the public API for URL validation. Call this before any operation
/// that might be influenced by a URL (e.g., cache lookups) to prevent attacks.
///
/// # Validation Steps
///
/// 1. Parse URL and validate scheme (http/https only)
/// 2. Check for localhost hostnames
/// 3. If IP literal: check against private ranges
/// 4. If hostname: resolve DNS and check all returned IPs
///
/// # Errors
///
/// Returns an error if:
/// - URL is malformed
/// - Scheme is not http or https
/// - Host is localhost or a local hostname
/// - IP address (literal or resolved) is private/reserved
/// - DNS resolution fails
///
/// # Example
///
/// ```ignore
/// // Safe URL - passes validation
/// validate_url_ssrf("https://example.com/page").await?;
///
/// // Blocked - private IP
/// validate_url_ssrf("http://192.168.1.1/admin").await; // Error
///
/// // Blocked - localhost
/// validate_url_ssrf("http://localhost:8080/api").await; // Error
/// ```
pub async fn validate_url_ssrf(url: &str) -> Result<()> {
    let _ = validate_url_ssrf_and_resolve(url).await?;
    Ok(())
}

/// Validates URL for SSRF and returns a pinned address for DNS rebinding mitigation.
///
/// This internal function performs full SSRF validation and, for hostname URLs,
/// returns a resolved `SocketAddr` that can be pinned to the HTTP client. This
/// prevents DNS rebinding attacks where an attacker's DNS server returns a
/// public IP for the initial check but a private IP for the actual request.
///
/// # Returns
///
/// - `Ok(None)` - URL uses an IP literal (no pinning needed)
/// - `Ok(Some((host, addr)))` - Hostname with pinned resolved address
/// - `Err(...)` - Validation failed
///
/// The returned `(host, SocketAddr)` pair can be passed to `reqwest::ClientBuilder::resolve()`
/// to ensure the HTTP request uses the validated address.
async fn validate_url_ssrf_and_resolve(url: &str) -> Result<Option<(String, SocketAddr)>> {
    let parsed = Url::parse(url)?;

    // Only allow http and https schemes
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(anyhow!(
                "URL scheme '{scheme}' not allowed (only http/https permitted)"
            ));
        }
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("URL must have a valid host"))?;

    // Reject localhost variations
    if host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("localhost.localdomain")
    {
        return Err(anyhow!("Cannot fetch from localhost"));
    }

    // If hostname is an IP address, check if it's private and skip DNS pinning.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(anyhow!("Cannot fetch from private IP address: {ip}"));
        }
        return Ok(None);
    }

    // Resolve the host and ensure it does not map to a private/reserved IP.
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow!("unknown default port"))?;

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("DNS resolution failed for host '{host}':"))?;

    let mut chosen: Option<SocketAddr> = None;
    for addr in addrs {
        if is_private_ip(addr.ip()) {
            return Err(anyhow!(
                "Resolved host '{host}' maps to a private/reserved IP; refusing to fetch"
            ));
        }
        if chosen.is_none() {
            chosen = Some(addr);
        }
    }

    let chosen = chosen.ok_or_else(|| anyhow!("DNS resolution returned no addresses"))?;
    Ok(Some((host.to_string(), chosen)))
}

// ============================================================================
// robots.txt Compliance
// ============================================================================

/// Constructs the robots.txt URL for a given page URL.
///
/// Extracts the origin (scheme + host + port) and appends `/robots.txt`.
/// Handles IPv4, IPv6 (with brackets), and domain hostnames.
fn get_robots_url(url: &str) -> Result<String> {
    let parsed = Url::parse(url)?;
    let scheme = parsed.scheme();
    let host = parsed.host().ok_or_else(|| anyhow!("no host in URL"))?;
    let host_str = match host {
        Host::Domain(d) => d.to_string(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => format!("[{ip}]"),
    };
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{scheme}://{host_str}{port}/robots.txt"))
}

/// Fetches and caches robots.txt content for a domain.
///
/// ## Caching Behavior
///
/// - Results are cached by origin (scheme://host:port)
/// - Both successful fetches AND 404s are cached (None = no robots.txt)
/// - Cache is bounded to 1024 entries; cleared entirely when full
/// - Cache uses `RwLock` for concurrent read access
///
/// ## Return Values
///
/// - `Ok(Some(content))` - robots.txt exists and was fetched
/// - `Ok(None)` - No robots.txt (404 or fetch error) - means "allow all"
/// - `Err(...)` - URL parsing error
async fn get_robots_content(client: &Client, url: &str) -> Result<Option<String>> {
    let robots_url = get_robots_url(url)?;
    let parsed = Url::parse(url)?;
    let host = parsed.host().ok_or_else(|| anyhow!("no host in URL"))?;
    let host_str = match host {
        Host::Domain(d) => d.to_string(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => format!("[{ip}]"),
    };
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow!("unknown default port for scheme"))?;
    let domain = format!("{}://{}:{}", parsed.scheme(), host_str, port);

    // Check cache first (read lock)
    {
        let cache = ROBOTS_CACHE.read().await;
        if let Some(ref map) = *cache
            && let Some(cached) = map.get(&domain)
        {
            return Ok(cached.clone());
        }
    }

    // Fetch robots.txt
    let response = client
        .get(&robots_url)
        .timeout(Duration::from_secs(5))
        .header(header::USER_AGENT, USER_AGENT)
        .send()
        .await;

    let content = match response {
        Ok(resp) if resp.status().is_success() => Some(resp.text().await?),
        _ => None, // No robots.txt or error = allow all
    };

    // Cache the result (write lock)
    {
        let mut cache = ROBOTS_CACHE.write().await;
        if cache.is_none() {
            *cache = Some(HashMap::new());
        }
        if let Some(map) = cache.as_mut() {
            if map.len() >= ROBOTS_CACHE_MAX_ENTRIES && !map.contains_key(&domain) {
                // Coarse eviction: clear to prevent unbounded growth.
                map.clear();
            }
            map.insert(domain, content.clone());
        }
    }

    Ok(content)
}

/// Checks if a URL path is allowed by the domain's robots.txt.
///
/// Uses the `robotstxt` crate's `DefaultMatcher` which implements the
/// standard robots.txt matching algorithm. Matches against our user agent
/// (`tools-webfetch/0.1`).
///
/// # Returns
///
/// - `Ok(true)` - URL is allowed (or no robots.txt exists)
/// - `Ok(false)` - URL is disallowed by robots.txt
/// - `Err(...)` - URL parsing error
async fn is_allowed_by_robots(client: &Client, url: &str) -> Result<bool> {
    let content = get_robots_content(client, url).await?;

    match content {
        None => Ok(true), // No robots.txt = allow all (per specification)
        Some(txt) => {
            let mut matcher = DefaultMatcher::default();
            let parsed = Url::parse(url)?;
            let path = robots_match_path(&parsed);
            Ok(matcher.one_agent_allowed_by_robots(&txt, USER_AGENT, &path))
        }
    }
}

fn robots_match_path(parsed: &Url) -> String {
    match parsed.query() {
        Some(query) => format!("{}?{}", parsed.path(), query),
        None => parsed.path().to_string(),
    }
}

// ============================================================================
// Document Fetching
// ============================================================================

/// Fetches a remote document with full SSRF protection and robots.txt compliance.
///
/// This is the main HTTP fetching function. It implements:
///
/// ## Security
///
/// - SSRF validation on initial URL AND every redirect hop
/// - Address pinning to prevent DNS rebinding attacks
/// - robots.txt compliance check before fetching
/// - Maximum response size limit (25 MiB) to prevent OOM/DoS
///
/// ## Redirect Handling
///
/// Redirects are followed manually (not by reqwest's automatic redirect)
/// to ensure SSRF validation occurs on each hop. This prevents attacks
/// where an allowed URL redirects to a private IP.
///
/// Maximum 5 redirects are followed before failing.
///
/// ## Request Headers
///
/// - `User-Agent`: `tools-webfetch/0.1`
/// - `Accept`: Prefers HTML, falls back to XML/plain text
/// - `Cache-Control`/`Pragma`: Set to `no-cache` if `req.no_cache` is true
///
/// # Errors
///
/// Returns an error if:
/// - URL fails SSRF validation (blocked scheme, private IP, etc.)
/// - URL is disallowed by robots.txt
/// - HTTP request fails (network error, timeout)
/// - HTTP response is 404 or other error status
/// - Too many redirects (> 5)
/// - Response size exceeds `MAX_RESPONSE_BYTES`
pub async fn fetch_document(req: &FetchRequest) -> Result<FetchedBody> {
    let mut current_url = req.url.clone();
    let mut redirects_followed = 0usize;

    loop {
        // Validate URL for SSRF protection on every hop, and pin a validated address when possible.
        let resolve = validate_url_ssrf_and_resolve(&current_url).await?;
        let pinned_client = build_http_client_with_resolve(resolve.as_ref())?;

        // Check robots.txt
        if !is_allowed_by_robots(&pinned_client, &current_url).await? {
            return Err(anyhow!("URL disallowed by robots.txt: {current_url}"));
        }

        let mut builder = pinned_client
            .get(&current_url)
            .timeout(DEFAULT_TIMEOUT)
            .header(header::USER_AGENT, USER_AGENT)
            .header(
                header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,text/plain;q=0.8,*/*;q=0.7",
            );

        if req.no_cache {
            builder = builder
                .header(header::CACHE_CONTROL, "no-cache")
                .header(header::PRAGMA, "no-cache");
        }

        let mut response = builder.send().await?;
        let status = response.status();

        // Handle redirects manually
        if status.is_redirection() {
            if redirects_followed >= MAX_REDIRECTS {
                return Err(anyhow!(
                    "Too many redirects (>{}) when fetching {}",
                    MAX_REDIRECTS,
                    req.url
                ));
            }

            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect without Location header from {current_url}"))?;

            let base = Url::parse(&current_url)?;
            let next = base.join(location).with_context(|| {
                format!("invalid redirect Location '{location}' from {current_url}")
            })?;
            current_url = next.to_string();
            redirects_followed += 1;
            continue;
        }

        if status == StatusCode::NOT_FOUND {
            return Err(anyhow!("HTTP 404: resource not found"));
        } else if !status.is_success() {
            return Err(anyhow!("http error {status} when fetching {current_url}"));
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(std::string::ToString::to_string);
        let fetched_at = Utc::now();

        // Bounded body read to prevent OOM from oversized web pages.
        let mut bytes = Vec::new();
        let mut total_read = 0usize;
        while let Some(chunk) = response.chunk().await? {
            if total_read + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(anyhow!(
                    "Response from {} exceeds maximum allowed size ({} MiB)",
                    current_url,
                    MAX_RESPONSE_BYTES / 1024 / 1024
                ));
            }
            bytes.extend_from_slice(&chunk);
            total_read += chunk.len();
        }

        return Ok(FetchedBody {
            body: bytes,
            content_type,
            fetched_at,
        });
    }
}

fn build_http_client_with_resolve(resolve: Option<&(String, SocketAddr)>) -> Result<Client> {
    let mut builder = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(DEFAULT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none()) // Manual redirect for SSRF validation
        .brotli(true)
        .gzip(true)
        .deflate(true);
    if let Some((host, addr)) = resolve {
        builder = builder.resolve(host, *addr);
    }
    Ok(builder.build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use robotstxt::DefaultMatcher;

    #[tokio::test]
    async fn validate_url_ssrf_blocks_non_http_schemes() {
        let err = validate_url_ssrf("file:///etc/passwd")
            .await
            .expect_err("expected SSRF validation to reject file://");
        assert!(err.to_string().contains("not allowed"));
    }

    #[tokio::test]
    async fn validate_url_ssrf_blocks_localhost_hostname() {
        let err = validate_url_ssrf("http://localhost:8080/")
            .await
            .expect_err("expected SSRF validation to reject localhost");
        assert!(err.to_string().to_ascii_lowercase().contains("localhost"));
    }

    #[tokio::test]
    async fn validate_url_ssrf_blocks_private_ip_literal() {
        let err = validate_url_ssrf("http://192.168.1.10/")
            .await
            .expect_err("expected SSRF validation to reject private IP");
        assert!(err.to_string().contains("private IP"));

        // Multicast range
        let err = validate_url_ssrf("http://224.0.0.1/")
            .await
            .expect_err("expected SSRF validation to reject multicast IP");
        assert!(err.to_string().contains("private IP"));

        // Reserved range
        let err = validate_url_ssrf("http://240.0.0.1/")
            .await
            .expect_err("expected SSRF validation to reject reserved IP");
        assert!(err.to_string().contains("private IP"));
    }

    #[tokio::test]
    async fn validate_url_ssrf_allows_public_ip_literal_without_dns() {
        // example.com (93.184.216.34) - using an IP literal avoids DNS in tests.
        validate_url_ssrf("https://93.184.216.34/")
            .await
            .expect("expected public IP literal to pass SSRF validation");
    }

    #[test]
    fn robots_match_path_includes_query_string() {
        let parsed = Url::parse("https://example.com/docs/page?print=1&lang=en").expect("url");
        assert_eq!(robots_match_path(&parsed), "/docs/page?print=1&lang=en");
    }

    #[test]
    fn robots_match_path_without_query_is_path_only() {
        let parsed = Url::parse("https://example.com/docs/page").expect("url");
        assert_eq!(robots_match_path(&parsed), "/docs/page");
    }

    #[test]
    fn robots_match_path_includes_query() {
        let parsed = Url::parse("https://example.com/search?q=secret").expect("valid URL");
        assert_eq!(robots_match_path(&parsed), "/search?q=secret");
    }

    #[test]
    fn robots_matcher_can_block_query_rule() {
        let parsed = Url::parse("https://example.com/search?q=secret").expect("valid URL");
        let robots = "User-agent: *\nDisallow: /search?q=secret\n";
        let mut matcher = DefaultMatcher::default();
        assert!(!matcher.one_agent_allowed_by_robots(
            robots,
            USER_AGENT,
            &robots_match_path(&parsed)
        ));
    }
}
