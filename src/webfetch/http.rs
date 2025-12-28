//! HTTP fetching with SSRF protection and robots.txt compliance.
//!
//! This module provides secure HTTP fetching capabilities with multiple layers
//! of protection against Server-Side Request Forgery (SSRF) attacks and
//! automatic robots.txt compliance.
//!
//! ## Security Features
//!
//! ### SSRF Protection
//!
//! The module implements comprehensive SSRF validation:
//!
//! 1. **Scheme validation**: Only `http://` and `https://` are allowed.
//!    Blocks `file://`, `ftp://`, `gopher://`, etc.
//!
//! 2. **Hostname validation**: Blocks `localhost`, `localhost.localdomain`,
//!    and similar local hostname variations.
//!
//! 3. **IP address validation**: Blocks private and reserved IP ranges:
//!    - `10.0.0.0/8` (private)
//!    - `172.16.0.0/12` (private)
//!    - `192.168.0.0/16` (private)
//!    - `127.0.0.0/8` (loopback)
//!    - `169.254.0.0/16` (link-local)
//!    - `100.64.0.0/10` (carrier-grade NAT)
//!    - IPv6 equivalents (::1, fc00::/7, fe80::/10)
//!
//! 4. **DNS resolution validation**: Resolves hostnames and checks all
//!    returned IPs. Blocks if ANY resolved IP is private/reserved.
//!    This prevents DNS rebinding attacks where `evil.com` resolves to `127.0.0.1`.
//!
//! 5. **Address pinning**: The resolved address is pinned for the HTTP request
//!    to prevent time-of-check/time-of-use (TOCTOU) attacks.
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
//! 2. Uses `tools-mcp-webfetch/0.1` as the user agent for matching
//! 3. Rejects requests to disallowed paths with an error
//! 4. Missing robots.txt = allow all (per specification)
//!
//! ## Usage
//!
//! ```ignore
//! use tools_mcp::webfetch::http::{fetch_document, validate_url_ssrf};
//!
//! // Validate URL before any operation
//! validate_url_ssrf("https://example.com/page").await?;
//!
//! // Fetch document with full protection
//! let request = FetchRequest { url: "https://example.com".into(), ..Default::default() };
//! let body = fetch_document(&request).await?;
//! ```

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::{header, Client, StatusCode};
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
const USER_AGENT: &str = "tools-mcp-webfetch/0.1";

/// Default timeout for HTTP requests (covers connect + response).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// Maximum redirect hops before failing (prevents redirect loops).
const MAX_REDIRECTS: usize = 5;

/// Maximum cached robots.txt entries before eviction.
/// Uses simple clear-on-full strategy to bound memory usage.
const ROBOTS_CACHE_MAX_ENTRIES: usize = 1024;

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
                || ipv4.octets()[0] == 0   // 0/8 "This network"
                || ipv4.octets()[0] == 100 && (ipv4.octets()[1] & 0xC0 == 0x40) // 100.64/10 CGNAT
        }
        IpAddr::V6(ipv6) => {
            // IPv4-mapped addresses (::ffff:x.x.x.x) must be checked as IPv4
            if let Some(v4) = ipv6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            ipv6.is_loopback()      // ::1
                || ipv6.is_unspecified() // ::
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
                "URL scheme '{}' not allowed (only http/https permitted)",
                scheme
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
            return Err(anyhow!("Cannot fetch from private IP address: {}", ip));
        }
        return Ok(None);
    }

    // Resolve the host and ensure it does not map to a private/reserved IP.
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow!("unknown default port"))?;

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("DNS resolution failed for host '{}':", host))?;

    let mut chosen: Option<SocketAddr> = None;
    for addr in addrs {
        if is_private_ip(addr.ip()) {
            return Err(anyhow!(
                "Resolved host '{}' maps to a private/reserved IP; refusing to fetch",
                host
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
        Host::Ipv6(ip) => format!("[{}]", ip),
    };
    let port = parsed.port().map(|p| format!(":{}", p)).unwrap_or_default();
    Ok(format!("{}://{}{}/robots.txt", scheme, host_str, port))
}

/// Fetches and caches robots.txt content for a domain.
///
/// ## Caching Behavior
///
/// - Results are cached by origin (scheme://host:port)
/// - Both successful fetches AND 404s are cached (None = no robots.txt)
/// - Cache is bounded to 1024 entries; cleared entirely when full
/// - Cache uses RwLock for concurrent read access
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
        Host::Ipv6(ip) => format!("[{}]", ip),
    };
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow!("unknown default port for scheme"))?;
    let domain = format!("{}://{}:{}", parsed.scheme(), host_str, port);

    // Check cache first (read lock)
    {
        let cache = ROBOTS_CACHE.read().await;
        if let Some(ref map) = *cache {
            if let Some(cached) = map.get(&domain) {
                return Ok(cached.clone());
            }
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
/// (`tools-mcp-webfetch/0.1`).
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
            let path = parsed.path();
            Ok(matcher.one_agent_allowed_by_robots(&txt, USER_AGENT, path))
        }
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
/// - `User-Agent`: `tools-mcp-webfetch/0.1`
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
pub async fn fetch_document(req: &FetchRequest) -> Result<FetchedBody> {
    let mut current_url = req.url.clone();

    for _ in 0..=MAX_REDIRECTS {
        // Validate URL for SSRF protection on every hop, and pin a validated address when possible.
        let resolve = validate_url_ssrf_and_resolve(&current_url).await?;
        let pinned_client = build_http_client_with_resolve(resolve.as_ref())?;

        // Check robots.txt
        if !is_allowed_by_robots(&pinned_client, &current_url).await? {
            return Err(anyhow!("URL disallowed by robots.txt: {}", current_url));
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

        let response = builder.send().await?;
        let status = response.status();

        // Handle redirects manually
        if status.is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("redirect without Location header from {}", current_url))?;

            let base = Url::parse(&current_url)?;
            let next = base.join(location).with_context(|| {
                format!(
                    "invalid redirect Location '{}' from {}",
                    location, current_url
                )
            })?;
            current_url = next.to_string();
            continue;
        }

        if status == StatusCode::NOT_FOUND {
            return Err(anyhow!("HTTP 404: resource not found"));
        } else if !status.is_success() {
            return Err(anyhow!(
                "http error {} when fetching {}",
                status,
                current_url
            ));
        }

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let fetched_at = Utc::now();
        let bytes = response.bytes().await?;
        return Ok(FetchedBody {
            body: bytes.to_vec(),
            content_type,
            fetched_at,
        });
    }

    Err(anyhow!(
        "Too many redirects (>{}) when fetching {}",
        MAX_REDIRECTS,
        req.url
    ))
}

/// Construct a shared HTTP client configured for MCP WebFetch usage.
/// Redirects are disabled - they're followed manually in fetch_document for SSRF protection.
#[allow(dead_code)]
pub fn build_http_client() -> Result<Client> {
    build_http_client_with_resolve(None)
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
