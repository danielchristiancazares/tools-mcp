use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode, header};
use robotstxt::DefaultMatcher;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;
use tokio::sync::RwLock;
use url::Url;

use crate::webfetch::types::FetchRequest;

const USER_AGENT: &str = "tools-mcp-webfetch/0.1";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;

// Global cache for robots.txt content per domain (async-safe RwLock)
static ROBOTS_CACHE: RwLock<Option<HashMap<String, Option<String>>>> = RwLock::const_new(None);

/// Raw payload returned from the HTTP fetch layer.
pub struct FetchedBody {
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub fetched_at: DateTime<Utc>,
}

/// Check if an IP address is private/reserved (SSRF protection)
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            ipv4.is_private()
                || ipv4.is_loopback()
                || ipv4.is_link_local()
                || ipv4.is_broadcast()
                || ipv4.is_documentation()
                || ipv4.is_unspecified()
                // Block additional reserved ranges
                || ipv4.octets()[0] == 0      // "This network"
                || ipv4.octets()[0] == 100 && (ipv4.octets()[1] & 0xC0 == 0x40) // Carrier-grade NAT
        }
        IpAddr::V6(ipv6) => {
            // Handle IPv4-mapped/compatible IPv6 addresses (e.g., ::ffff:127.0.0.1)
            if let Some(v4) = ipv6.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(v4));
            }
            ipv6.is_loopback()
                || ipv6.is_unspecified()
                || ((ipv6.segments()[0] & 0xfe00) == 0xfc00) // Unique local
                || ((ipv6.segments()[0] & 0xffc0) == 0xfe80) // Link-local
        }
    }
}

/// Validate URL for SSRF protection
// Perform conservative SSRF validation:
// - allow only http/https schemes
// - disallow localhost names and literal private IPs
// - resolve DNS and block if any resolved IP is private/reserved
// Note: DNS resolution uses Tokio's async resolver [tokio v1.x, lookup_host]
// https://docs.rs/tokio/1/tokio/net/fn.lookup_host.html
pub async fn validate_url_ssrf(url: &str) -> Result<()> {
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

    // Check hostname
    if let Some(host) = parsed.host_str() {
        // Reject localhost variations
        if host.eq_ignore_ascii_case("localhost")
            || host.eq_ignore_ascii_case("localhost.localdomain")
        {
            return Err(anyhow!("Cannot fetch from localhost"));
        }

        // If hostname is an IP address, check if it's private
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_private_ip(ip) {
                return Err(anyhow!("Cannot fetch from private IP address: {}", ip));
            }
        }
    } else {
        return Err(anyhow!("URL must have a valid host"));
    }

    // Resolve the host and ensure it does not map to a private/reserved IP
    // Choose a sensible default port for resolution when none is present
    let port = parsed.port().unwrap_or_else(|| match parsed.scheme() {
        "https" => 443,
        _ => 80,
    });
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("URL must have a valid host"))?;
    let mut any_private = false;
    // Use Tokio DNS resolution [tokio v1.x]
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .with_context(|| format!("DNS resolution failed for host '{}':", host))?;
    for addr in addrs {
        if is_private_ip(addr.ip()) {
            any_private = true;
            break;
        }
    }
    if any_private {
        return Err(anyhow!(
            "Resolved host '{}' maps to a private/reserved IP; refusing to fetch",
            host
        ));
    }

    Ok(())
}

/// Get the base domain from a URL for robots.txt lookup
fn get_robots_url(url: &str) -> Result<String> {
    let parsed = Url::parse(url)?;
    let scheme = parsed.scheme();
    let host = parsed.host_str().ok_or_else(|| anyhow!("no host in URL"))?;
    let port = parsed.port().map(|p| format!(":{}", p)).unwrap_or_default();
    Ok(format!("{}://{}{}/robots.txt", scheme, host, port))
}

/// Fetch robots.txt content for a domain, with caching
async fn get_robots_content(client: &Client, url: &str) -> Result<Option<String>> {
    let robots_url = get_robots_url(url)?;
    let parsed = Url::parse(url)?;
    let domain = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));

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
        cache.as_mut().unwrap().insert(domain, content.clone());
    }

    Ok(content)
}

/// Check if a URL is allowed by robots.txt
async fn is_allowed_by_robots(client: &Client, url: &str) -> Result<bool> {
    let content = get_robots_content(client, url).await?;

    match content {
        None => Ok(true), // No robots.txt = allow all
        Some(txt) => {
            let mut matcher = DefaultMatcher::default();
            let parsed = Url::parse(url)?;
            let path = parsed.path();
            Ok(matcher.one_agent_allowed_by_robots(&txt, USER_AGENT, path))
        }
    }
}

/// Fetch the remote document, applying cache-busting headers when requested.
/// Manually follows redirects with SSRF validation on each hop to prevent redirect-based SSRF attacks.
pub async fn fetch_document(client: &Client, req: &FetchRequest) -> Result<FetchedBody> {
    let mut current_url = req.url.clone();

    for _ in 0..=MAX_REDIRECTS {
        // Validate URL for SSRF protection on every hop
        validate_url_ssrf(&current_url).await?;

        // Check robots.txt
        if !is_allowed_by_robots(client, &current_url).await? {
            return Err(anyhow!("URL disallowed by robots.txt: {}", current_url));
        }

        let mut builder = client
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
            let next = base
                .join(location)
                .with_context(|| format!("invalid redirect Location '{}' from {}", location, current_url))?;
            current_url = next.to_string();
            continue;
        }

        if status == StatusCode::NOT_FOUND {
            return Err(anyhow!("HTTP 404: resource not found"));
        } else if !status.is_success() {
            return Err(anyhow!("http error {} when fetching {}", status, current_url));
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
pub fn build_http_client() -> Result<Client> {
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(DEFAULT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none()) // Manual redirect for SSRF validation
        .brotli(true)
        .gzip(true)
        .deflate(true)
        .build()?;
    Ok(client)
}
