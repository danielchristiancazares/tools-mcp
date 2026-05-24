# SDD: WebFetch

**Date:** 2026-05-24
**Scope:** Design contract for the `WebFetch` MCP tool.
**Source:** `tools-mcp-webfetch/src/webfetch_tool.rs`

---

## 1. Normative Language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY, and OPTIONAL in this document are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when, they appear in all capitals.

## 2. Self-Containment

1. This document is the authoritative design contract for the `WebFetch` tool.
2. External references are contextual only. All normative requirements for this tool are restated in this file.
3. If implementation diverges from this document, the divergence MUST be reconciled in favor of whichever side is correct on the merits.

## 3. Scope

### 3.1 What This Document Covers

`WebFetch` is the MCP tool that fetches a URL over HTTP, optionally re-renders it through a headless Chrome/Chromium browser, converts the result to Markdown, splits the Markdown into token-budgeted chunks, and returns a structured response. It enforces SSRF protection, `robots.txt` policy, and a maximum response size on every fetch path. The tool is owned by the `tools-mcp-webfetch` crate; the entry point is `handle_webfetch` in `tools-mcp-webfetch/src/webfetch_tool.rs:8`, which delegates to `crate::webfetch::run_fetch` via `default_web_fetcher().fetch(...)` (`tools-mcp-webfetch/src/services.rs`).

### 3.2 Explicitly Out of Scope

- JSON-RPC framing and method routing (covered in `docs/protocol.md`).
- Tool-registry composition (covered in `docs/architecture.md`).
- Cross-cutting environment variables (full catalog in `docs/configuration.md`).
- Threat model and adversary assumptions (covered in `docs/security.md`).
- Other tools that share infrastructure: none. `WebFetch` does not share state with other tools beyond the process-global browser pool and the on-disk cache, both of which are private to `tools-mcp-webfetch`.

## 4. Design Contract

### 4.1 Registration

| Property | Value |
|---|---|
| MCP tool name | `WebFetch` |
| Aliases | None |
| Registration gate | Always registered (no env gate on registration; see §4.2 for the runtime browser gate) |
| Owning crate | `tools-mcp-webfetch` |
| Handler function | `handle_webfetch` (`tools-mcp-webfetch/src/webfetch_tool.rs:8`) |
| Schema definition | `tools-mcp-webfetch/src/tools.rs:4` |
| Registration call | `tools-mcp-webfetch/src/lib.rs:12` |

### 4.2 Invariants

The following invariants MUST hold on every invocation:

- **SSRF before cache** — Every fetch path MUST call `validate_url_ssrf_and_resolve` *before* any cache lookup, both for HTTP (`tools-mcp-webfetch/src/webfetch/mod.rs:163`) and browser (`tools-mcp-webfetch/src/webfetch/mod.rs:279`) paths. This prevents a cache entry from bypassing SSRF policy on a URL that would now resolve to a private address.
- **SSRF on every redirect hop** — `fetch_document` MUST revalidate SSRF on each redirect hop (`tools-mcp-webfetch/src/webfetch/http.rs:564`); the HTTP client MUST have automatic redirects disabled (`tools-mcp-webfetch/src/webfetch/http.rs:655`).
- **DNS pinning** — For hostname URLs, the resolved `SocketAddr` MUST be pinned via `reqwest::ClientBuilder::resolve` for the duration of the request (`tools-mcp-webfetch/src/webfetch/http.rs:659`) to prevent DNS rebinding between the validation check and the actual fetch.
- **robots.txt enforcement** — `ensure_fetch_allowed` MUST refuse the fetch when `robots.txt` disallows the URL *or* when the `robots.txt` endpoint returns a 5xx or is unreachable (`tools-mcp-webfetch/src/webfetch/http.rs:502`). 4xx responses are treated as "no `robots.txt` exists" and allow the fetch per RFC.
- **Browser gate is hard-fail-closed** — Any browser-rendering attempt MUST return an error before launching Chrome unless `WEBFETCH_ENABLE_BROWSER_UNSAFE=true` is set in the environment (`tools-mcp-webfetch/src/webfetch/mod.rs:285`). This applies to whitelist matches, heuristic-triggered fallbacks, and `force_browser=true` requests.
- **Response size cap** — The HTTP body reader MUST abort with an error if the response exceeds 25 MiB (`MAX_RESPONSE_BYTES`, `tools-mcp-webfetch/src/webfetch/http.rs:82,632`).
- **Cache key includes rendering method** — Cache keys MUST differ between HTTP and browser fetches (`_http` vs `_browser` suffix, `tools-mcp-webfetch/src/webfetch/mod.rs:168,302`) so an HTTP-rendered shell cannot be served when a browser render is needed.
- **No panic on failure** — The handler MUST translate every error path into `ToolCallOutcome::err_with` (`tools-mcp-webfetch/src/webfetch_tool.rs:35`); it MUST NOT panic.
- **Error diagnostics are redacted** — On error, the `details` field returned to the caller MUST be the redacted constant string for the classified `error_type` (`tools-mcp-webfetch/src/webfetch_tool.rs:33,176`). Raw network, browser, and DNS diagnostics MUST NOT leak through the tool response.

### 4.3 Explicitly Forbidden Shapes

- MUST NOT bypass `validate_url_ssrf_and_resolve` for any URL or any redirect hop.
- MUST NOT issue a browser render without checking `WEBFETCH_ENABLE_BROWSER_UNSAFE` first.
- MUST NOT execute fetched content as commands or pass it to a shell.
- MUST NOT serve a cached entry for a URL that fails current SSRF or `robots.txt` validation.
- MUST NOT silently fall back to HTTP when `force_browser=true` and the browser path fails — failure MUST propagate as an error so the caller can detect missing browser capability (`tools-mcp-webfetch/src/webfetch/mod.rs:150`).
- MUST NOT include raw upstream HTTP messages, raw browser stderr, or raw DNS resolution detail in the `details` field of the error response.

## 5. Design Goals

- **HTTP-first, browser only when needed.** HTTP fetching is faster, has a smaller attack surface, and avoids spawning a Chrome process. The whitelist and heuristic fallback exist to recover JavaScript-rendered content, not to make browser rendering the default.
- **Fail closed on security checks.** SSRF, `robots.txt`, and browser-enable gates default to refusing the fetch rather than degrading silently. Operators must opt in explicitly to relax any of them.
- **Token-aware output.** Chunking aligned to a tokenizer (`cl100k_base`) lets callers plan prompts deterministically rather than guessing at character counts.
- **Cache disjoint by rendering method.** A cached HTTP shell must not be served when the caller (implicitly via heuristics, or explicitly via `force_browser`) needs browser-rendered output.

## 6. Tool Specification

### 6.1 Input Schema

| Field | Type | Required | Default | Constraints | Description |
|---|---|---|---|---|---|
| `url` | string | Yes | — | Must be absolute `http(s)` URL | URL to fetch. |
| `max_chunk_tokens` | integer | No | `600` | `>= 1` | Approximate token budget per chunk, counted with `cl100k_base`. |
| `no_cache` | boolean | No | `false` | — | When `true`, bypass the on-disk cache and force a fresh fetch. Fresh content is still written to cache for future requests. |
| `force_browser` | boolean | No | `false` | — | When `true`, attempt headless browser rendering regardless of heuristics. Subject to the `WEBFETCH_ENABLE_BROWSER_UNSAFE` gate; see §4.2 and §6.4 for failure semantics. |

The schema sets `"additionalProperties": false` (`tools-mcp-webfetch/src/tools.rs:30`); the deserializer sets `#[serde(deny_unknown_fields)]` (`tools-mcp-webfetch/src/webfetch/types.rs:24`). Unknown fields produce a tool-level error (`isError: true`) with text `"invalid arguments: ..."`.

> Schema source: `tools-mcp-webfetch/src/tools.rs:8-31`

### 6.2 Behavior

`run_fetch` (`tools-mcp-webfetch/src/webfetch/mod.rs:140`) implements a tiered rendering pipeline. Each numbered step lists the file:line for verification.

1. **Decide browser-first vs HTTP-first** — Set `use_browser = req.force_browser || whitelist::is_whitelisted_js_heavy(&req.url)` (`mod.rs:142`). The whitelist contains 22 domain patterns enumerated in §6.5.
2. **If browser-first: attempt browser render** (`mod.rs:144-158`)
   1. Call `try_browser_render(&req)` (`mod.rs:148`).
   2. On success, return the response.
   3. On failure when `force_browser=true`: return the wrapped error (`forced browser rendering failed`).
   4. On failure when `force_browser=false`: log a warning and fall through to the HTTP path.
3. **HTTP path: SSRF + robots validation** — Call `http::ensure_fetch_allowed(&req.url)` (`mod.rs:163`). This validates SSRF and refuses if `robots.txt` disallows the URL or returns a 5xx/unreachable error.
4. **HTTP path: cache lookup** — Compute key `format!("{}_http", req.url)` (`mod.rs:168`). When `no_cache=false`, read the cache; on hit, skip the network. Cache entries expire after `WEBFETCH_CACHE_TTL_SECONDS` seconds (default 24h, `cache.rs:64`). Cache total size is capped at `WEBFETCH_CACHE_MAX_BYTES` bytes (default 100 MiB, `cache.rs:69`); per-entry max is 25 MiB (`cache.rs:59`).
5. **HTTP path: fetch document** — Call `http::fetch_document(&req)` (`mod.rs:178`). Builds a hardened client with `tools-webfetch/0.1` user agent (`http.rs:65`), 20-second timeout (`http.rs:68`), no automatic redirects (`http.rs:655`), and pinned DNS. Follows up to 5 redirects manually (`http.rs:71`), revalidating SSRF on each hop. Reads at most 25 MiB of body bytes.
6. **HTTP path: extract Markdown** — Call `extract::extract` (`mod.rs:198`) to convert HTML to Markdown via `htmd` with filter rules for `nav`, `footer`, `header`, `script`, and `style` elements and inline links formatted `[text](url)`.
7. **HTTP path: JS-heavy heuristic** — Unless `force_browser=true`, run `heuristics::analyze_js_heavy` (`mod.rs:206-219`). The heuristic combines weighted signals: empty SPA shell (0.5), high script density (0.25), framework signatures (0.3), small payload <5 KB (0.15), explicit `noscript` warning (0.5). Threshold: combined confidence ≥ 0.5 classifies the page as JS-heavy (`heuristics.rs:165`).
8. **HTTP path: browser fallback on JS-heavy** — If the heuristic fires, retry via `try_browser_render` (`mod.rs:229`). On failure, log a warning and continue with the HTTP result in degraded mode (rendering_method remains `"http"`).
9. **Browser path: gate check** — In `try_browser_render` (`mod.rs:277`), validate SSRF (`mod.rs:279`), then check `WEBFETCH_ENABLE_BROWSER_UNSAFE` (`mod.rs:285`). If unset or not `"true"`, return error `"Browser rendering disabled for SSRF hardening; set WEBFETCH_ENABLE_BROWSER_UNSAFE=true to override"`. This applies to whitelist matches, heuristic fallbacks, and `force_browser=true` requests alike.
10. **Browser path: robots check + cache lookup** — Run `ensure_fetch_allowed` (`mod.rs:297`), then check the browser-keyed cache (`{url}_browser`, `mod.rs:302`).
11. **Browser path: availability + render** — `BrowserPool::is_available` performs a synchronous Chrome-binary search (env vars `CHROME_PATH`, `CHROMIUM_PATH`, `CHROME_EXECUTABLE`; then common install paths; then PATH via `which`/`where`; `browser.rs:710`). If absent, return error `"Chrome/Chromium not installed."`. Otherwise lazily initialize the global `BROWSER_POOL` (`mod.rs:327`) and call `render_page` (`mod.rs:332`).
12. **Render timing** — `render_page` wraps the full inner render in `NAVIGATION_TIMEOUT = 15s` (`browser.rs:74,338`). Inside that budget, `wait_for_network_idle` declares idle after `NETWORK_IDLE_TIMEOUT = 2s` of no new responses (`browser.rs:96,645`) and aborts the idle wait at `NETWORK_IDLE_MAX_WAIT = 5s` after navigation finishes (`browser.rs:99,652`). The browser pool restarts every 100 requests or after 1 hour of uptime (`browser.rs:66,70`). Resource blocking covers images, web fonts, and audio/video (`browser.rs:105-141`).
13. **Build response** — `build_response` (`mod.rs:379`) chunks the Markdown via `chunker::chunk_markdown` honoring `max_chunk_tokens` (default 600, `chunker.rs:40`), constructs the `note` field by joining `["cache_hit", "rendered_with_browser"]` if applicable with `", "` (`mod.rs:432`), and returns a `FetchResponse`.

### 6.3 Response Schema

**Success (`isError: false`):**

The MCP envelope wraps a serialized `FetchResponse` as JSON in `content[0].text` (`webfetch_tool.rs:22-27`). The handler does NOT add `FetchResponse` fields at the result top level; callers MUST parse `content[0].text` as JSON to recover the structured response.

```json
{
  "content": [
    {
      "type": "text",
      "text": "{\"url\":\"https://example.com/\",\"fetched_at\":\"2026-05-24T12:00:00Z\",\"title\":\"Example\",\"language\":\"en\",\"chunks\":[{\"heading\":null,\"text\":\"...\",\"token_count\":123}],\"rendering_method\":\"http\",\"note\":\"cache_hit\"}"
    }
  ],
  "isError": false
}
```

`FetchResponse` shape (`tools-mcp-webfetch/src/webfetch/types.rs:113`):

| Field | Type | Always present | Description |
|---|---|---|---|
| `url` | string | Yes | The URL that was fetched. |
| `fetched_at` | string | Yes | ISO 8601 UTC timestamp of original fetch (cache hits return the original fetch time, not the serve time). |
| `title` | string | No (omitted when `None`) | Title from `<title>` tag. |
| `language` | string | No | Language code from `<html lang>` or `<meta>`. |
| `chunks` | array | No (omitted when empty) | Token-budgeted chunks. Each entry: `{heading: string\|null, text: string, token_count: integer}`. |
| `rendering_method` | string | Yes | Either `"http"` or `"browser"`. |
| `note` | string | No | Comma-separated subset of `"cache_hit"`, `"rendered_with_browser"`. |

**Tool-level error (`isError: true`):**

The handler classifies every error path and emits a structured error response with redacted diagnostics (`webfetch_tool.rs:30-46`):

```json
{
  "content": [{"type": "text", "text": "<one-line message tailored to the error class>"}],
  "isError": true,
  "error_type": "ssrf_blocked",
  "url": "http://localhost/admin",
  "force_browser": false,
  "details": "The target URL was blocked by WebFetch SSRF protection before fetching. Raw network diagnostics are redacted from tool responses.",
  "remediation": ["Use a public http/https URL (...)", "If you need local content, read files from disk with Read instead of WebFetch."]
}
```

| Field | Type | Always present | Description |
|---|---|---|---|
| `content[0].text` | string | Yes | Human-readable error class summary. |
| `isError` | boolean | Yes | Always `true` on error. |
| `error_type` | string | Yes | One of the eight classified types listed in §6.4. |
| `url` | string | Yes | The URL the caller requested. |
| `force_browser` | boolean | Yes | Echoes the caller's `force_browser` argument. |
| `details` | string | Yes | Redacted, class-specific explanation. MUST NOT echo raw network/browser/DNS diagnostics. |
| `remediation` | array of string | Yes | Concrete recovery suggestions. |

### 6.4 Error Catalog

`classify_webfetch_error` (`tools-mcp-webfetch/src/webfetch_tool.rs:49`) maps internal anyhow chain text to one of nine `error_type` values:

| `error_type` | Trigger | Surfaced message (`content[0].text`) |
|---|---|---|
| `robots_disallowed` | `robots.txt` explicitly disallows the URL. | `WebFetch is blocked by robots.txt for this URL (the tool respects robots.txt).` |
| `robots_unavailable` | `robots.txt` endpoint returns 5xx or is unreachable. | `WebFetch could not verify robots.txt for this URL, so it refused to fetch it.` |
| `ssrf_blocked` | URL fails SSRF validation (scheme, localhost, private IP literal, DNS resolves to private). | `WebFetch blocked this URL for safety (SSRF protection). Only public http/https URLs are allowed; localhost/private IPs and non-http schemes are rejected.` |
| `browser_unavailable` | Chrome/Chromium binary not found, or `WEBFETCH_ENABLE_BROWSER_UNSAFE` gate refuses. (Note: the gate error string also matches the `browser_unavailable` path via the broader text classifier.) | `WebFetch could not use browser rendering because Chrome/Chromium is not available.` |
| `http_404` | Upstream returns HTTP 404. | `WebFetch got HTTP 404 (not found).` |
| `http_error` | Upstream returns a non-success status other than 404. | `WebFetch hit an HTTP error fetching the URL.` |
| `timeout` | Browser navigation timeout (15s), HTTP timeout (20s), or other timeout text in the anyhow chain. | `WebFetch timed out while fetching/rendering the URL.` |
| `network` | DNS or connectivity failure (text contains `dns`, `name or service`, or `failed to resolve`). | `WebFetch failed due to a DNS/network error.` |
| `unknown` | Any other failure. | `WebFetch failed.` |

`details` for each class is taken from a fixed-string mapping at `webfetch_tool.rs:176-205`; it always explains that raw diagnostics are redacted. The `remediation` vector is tailored per class. When `force_browser=true` and the error is `browser_unavailable`, the remediation appends `"Or set force_browser=false to attempt HTTP mode."` (`webfetch_tool.rs:103`).

### 6.5 Whitelisted JS-Heavy Domains

URLs matching any of these 22 patterns bypass HTTP-first and go directly to the browser path (subject to the `WEBFETCH_ENABLE_BROWSER_UNSAFE` gate). Source: `tools-mcp-webfetch/src/webfetch/whitelist.rs:37-78`.

| Category | Patterns |
|---|---|
| Framework docs | `react.dev`, `nextjs.org`, `vuejs.org`, `angular.dev`, `angular.io`, `svelte.dev` |
| Content platforms | `medium.com`, `notion.so`, `notion.site` |
| Developer platforms | `vercel.com`, `netlify.app`, `cloudflare.com` |
| Documentation platforms | `gitbook.io`, `readme.io`, `docusaurus.io` |
| Single-page apps | `app.slack.com`, `web.telegram.org`, `discord.com` |
| Hosting wildcards | `*.vercel.app`, `*.netlify.app`, `*.pages.dev`, `*.web.app` |

Match rules (`whitelist.rs:118`): an entry without leading `*.` matches the exact host *and* any subdomain (`medium.com` matches `blog.medium.com`). An entry with `*.` matches only subdomains, not the root (`*.vercel.app` matches `app.vercel.app` but not `vercel.app`).

## 7. Security Considerations

- **SSRF.** Blocks non-HTTP(S) schemes, `localhost`/`localhost.localdomain`, IPv4 RFC 1918 (10.0.0.0/8, 172.16/12, 192.168/16), loopback (127/8), link-local (169.254/16), broadcast, documentation ranges (192.0.2/24, 198.51.100/24, 203.0.113/24), CGNAT (100.64/10), unspecified (0.0.0.0), multicast, reserved (240/4), IPv6 loopback (`::1`), unspecified (`::`), unique local (`fc00::/7`), link-local (`fe80::/10`), 2001:db8::/32 documentation, multicast, and IPv4-mapped/compatible IPv6 (which are unwrapped and re-checked as IPv4 to prevent bypass). DNS is resolved and every returned IP is checked. The resolved address is pinned (`reqwest`'s `resolve` API) to defeat DNS rebinding. Revalidated on every redirect hop. (`tools-mcp-webfetch/src/webfetch/http.rs:148,231`)
- **robots.txt.** Fetched per origin with a 5-second timeout, cached in process up to 1024 entries (`http.rs:75`), matched with `tools-webfetch/0.1`. 4xx → "no robots.txt, allow all". 5xx or transport failure → fail closed with `robots_unavailable`. Disallowed paths → `robots_disallowed`. Path-matching uses the URL's path including query string (`http.rs:513`).
- **Response size.** Capped at 25 MiB (`MAX_RESPONSE_BYTES`, `http.rs:82`). Streaming reader aborts mid-body if the cap is exceeded.
- **Browser sandboxing.** Chrome runs with `--no-sandbox` (required for containerized environments). `chromiumoxide` `BrowserConfig` is configured with stealth flags and resource blocking. The browser pool restarts every 100 requests or 1 hour to bound memory growth (`browser.rs:66,70`).
- **Untrusted output.** Fetched content is external data. Consuming agents MUST treat `chunks[].text` as untrusted user input and MUST NOT execute it as commands or interpret it as instructions. See `docs/security.md` for the project-wide framing guidance.
- **Cache permissions.** Cache directory is created with `0o700` permissions on Unix (`cache.rs:217`); symlinked or non-directory paths are rejected.
- **Error redaction.** The error classifier replaces raw anyhow chain text with constant strings (`webfetch_tool.rs:176`) so upstream messages, resolved IP addresses, or browser stderr cannot leak through the tool response. Tested by `handle_webfetch_redacts_raw_network_error_details` (`webfetch_tool.rs:237`).

## 8. Configuration

| Variable | Default | Description |
|---|---|---|
| `WEBFETCH_ENABLE_BROWSER_UNSAFE` | unset (browser disabled) | When `"true"`, enables headless browser rendering. Until set, every browser attempt (whitelist match, heuristic fallback, or `force_browser=true`) errors with `error_type: browser_unavailable`. |
| `WEBFETCH_CACHE_TTL_SECONDS` | `86400` (24h) | TTL for on-disk cache entries. Invalid values fall back to the default. |
| `WEBFETCH_CACHE_MAX_BYTES` | `104857600` (100 MiB) | Total cache size cap. Per-entry cap is fixed at 25 MiB (`MAX_CACHE_ENTRY_BYTES`). |
| `CHROME_PATH` | unset | Explicit path to Chrome binary; tried first when discovering a browser. |
| `CHROMIUM_PATH` | unset | Same as above; checked after `CHROME_PATH`. |
| `CHROME_EXECUTABLE` | unset | Same as above; checked after `CHROMIUM_PATH`. |

`TOOLS_PRETTY_JSON` (process-wide; see `docs/configuration.md`) does NOT affect the `WebFetch` response shape because the handler builds the response object directly rather than using `ToolCallOutcome::ok_json_content`.

## 9. Code Anchors

| Claim | File | Line(s) |
|---|---|---|
| Tool registration | `tools-mcp-webfetch/src/lib.rs` | 12 |
| Tool name + schema | `tools-mcp-webfetch/src/tools.rs` | 4-31 |
| Handler entry point | `tools-mcp-webfetch/src/webfetch_tool.rs` | 8-47 |
| Request type (`deny_unknown_fields`) | `tools-mcp-webfetch/src/webfetch/types.rs` | 23-64 |
| Response type | `tools-mcp-webfetch/src/webfetch/types.rs` | 113-154 |
| Rendering pipeline (`run_fetch`) | `tools-mcp-webfetch/src/webfetch/mod.rs` | 140-251 |
| Browser path (`try_browser_render`) | `tools-mcp-webfetch/src/webfetch/mod.rs` | 277-362 |
| Browser gate (`WEBFETCH_ENABLE_BROWSER_UNSAFE`) | `tools-mcp-webfetch/src/webfetch/mod.rs` | 285-293 |
| `force_browser=true` hard-fail | `tools-mcp-webfetch/src/webfetch/mod.rs` | 150 |
| Response builder + `note` join | `tools-mcp-webfetch/src/webfetch/mod.rs` | 379-435 |
| HTTP timeout (20s) | `tools-mcp-webfetch/src/webfetch/http.rs` | 68 |
| Max redirects (5) | `tools-mcp-webfetch/src/webfetch/http.rs` | 71 |
| Max response bytes (25 MiB) | `tools-mcp-webfetch/src/webfetch/http.rs` | 82 |
| User agent | `tools-mcp-webfetch/src/webfetch/http.rs` | 65 |
| `is_private_ip` IPv4/IPv6 rules | `tools-mcp-webfetch/src/webfetch/http.rs` | 148-176 |
| SSRF validate + DNS pin | `tools-mcp-webfetch/src/webfetch/http.rs` | 231-300 |
| `ensure_fetch_allowed` | `tools-mcp-webfetch/src/webfetch/http.rs` | 502-511 |
| `fetch_document` + per-hop revalidation | `tools-mcp-webfetch/src/webfetch/http.rs` | 558-649 |
| `BrowserPool` lifecycle constants | `tools-mcp-webfetch/src/webfetch/browser.rs` | 64-99 |
| Chrome path env var override | `tools-mcp-webfetch/src/webfetch/browser.rs` | 710-720 |
| Resource blocking patterns | `tools-mcp-webfetch/src/webfetch/browser.rs` | 105-141 |
| Cache TTL + quota defaults | `tools-mcp-webfetch/src/webfetch/cache.rs` | 59-72 |
| Cache directory permissions (Unix `0o700`) | `tools-mcp-webfetch/src/webfetch/cache.rs` | 217 |
| Cache root (`<temp>/tools-webfetch`) | `tools-mcp-webfetch/src/webfetch/cache.rs` | 165-168 |
| JS-heavy heuristic weights + threshold | `tools-mcp-webfetch/src/webfetch/heuristics.rs` | 131-167 |
| Framework signature patterns | `tools-mcp-webfetch/src/webfetch/heuristics.rs` | 532-627 |
| JS-heavy whitelist (22 patterns) | `tools-mcp-webfetch/src/webfetch/whitelist.rs` | 37-78 |
| Whitelist match rules (wildcard semantics) | `tools-mcp-webfetch/src/webfetch/whitelist.rs` | 118-140 |
| Default chunk tokens (600) | `tools-mcp-webfetch/src/webfetch/chunker.rs` | 40 |
| `cl100k_base` tokenizer | `tools-mcp-webfetch/src/webfetch/chunker.rs` | 34,49 |
| Error classifier | `tools-mcp-webfetch/src/webfetch_tool.rs` | 49-174 |
| Error redaction map | `tools-mcp-webfetch/src/webfetch_tool.rs` | 176-205 |

## 10. Examples

### 10.1 Minimal request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "mcp/tools/call",
  "params": {
    "name": "WebFetch",
    "arguments": {"url": "https://example.com/"}
  }
}
```

### 10.2 Success response (HTTP path, cache miss)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"url\":\"https://example.com/\",\"fetched_at\":\"2026-05-24T12:00:00Z\",\"title\":\"Example Domain\",\"language\":\"en\",\"chunks\":[{\"heading\":null,\"text\":\"# Example Domain\\nThis domain is for use in illustrative examples...\",\"token_count\":42}],\"rendering_method\":\"http\"}"
      }
    ],
    "isError": false
  }
}
```

### 10.3 Success response (cache hit + browser render)

`note` joins both flags when applicable:

```json
{
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\"url\":\"https://medium.com/example\",\"fetched_at\":\"2026-05-24T10:00:00Z\",\"chunks\":[...],\"rendering_method\":\"browser\",\"note\":\"cache_hit, rendered_with_browser\"}"
      }
    ],
    "isError": false
  }
}
```

### 10.4 SSRF-blocked request (default config)

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "mcp/tools/call",
  "params": {
    "name": "WebFetch",
    "arguments": {"url": "http://localhost:8080/admin"}
  }
}
```

Response:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "content": [{"type": "text", "text": "WebFetch blocked this URL for safety (SSRF protection). Only public http/https URLs are allowed; localhost/private IPs and non-http schemes are rejected."}],
    "isError": true,
    "error_type": "ssrf_blocked",
    "url": "http://localhost:8080/admin",
    "force_browser": false,
    "details": "The target URL was blocked by WebFetch SSRF protection before fetching. Raw network diagnostics are redacted from tool responses.",
    "remediation": [
      "Use a public http/https URL (not file://, localhost, or a private IP).",
      "If you need local content, read files from disk with Read instead of WebFetch."
    ]
  }
}
```

### 10.5 `force_browser=true` without the browser gate set

With `WEBFETCH_ENABLE_BROWSER_UNSAFE` unset (the default), `force_browser=true` does NOT fall back to HTTP — it returns an error so the caller can detect the missing capability:

```json
{
  "result": {
    "content": [{"type": "text", "text": "WebFetch could not use browser rendering because Chrome/Chromium is not available."}],
    "isError": true,
    "error_type": "browser_unavailable",
    "url": "https://example.com/",
    "force_browser": true,
    "details": "Browser rendering is unavailable or disabled. Raw browser diagnostics are redacted from tool responses.",
    "remediation": [
      "Install Chrome/Chromium on the host running the MCP server, then retry.",
      "Or set force_browser=false to attempt HTTP mode."
    ]
  }
}
```

## 11. Testing

| Test | File | What it covers |
|---|---|---|
| `validate_url_ssrf_blocks_non_http_schemes` | `tools-mcp-webfetch/src/webfetch/http.rs:670` | Rejects `file://`. |
| `validate_url_ssrf_blocks_localhost_hostname` | `tools-mcp-webfetch/src/webfetch/http.rs:679` | Rejects `localhost`. |
| `validate_url_ssrf_blocks_private_ip_literal` | `tools-mcp-webfetch/src/webfetch/http.rs:687` | Rejects 192.168/16, 224.0.0.0/4, 240.0.0.0/4. |
| `validate_url_ssrf_blocks_ipv6_*` | `tools-mcp-webfetch/src/webfetch/http.rs:748-796` | Rejects IPv6 loopback, ULA, link-local, doc range, IPv4-mapped/compat loopback. |
| `validate_url_ssrf_allows_public_ip_literal_without_dns` | `tools-mcp-webfetch/src/webfetch/http.rs:706` | Allows public IPv4 literals. |
| `is_allowed_by_robots_blocks_disallowed_path` | `tools-mcp-webfetch/src/webfetch/http.rs:865` | Honors `Disallow` directives. |
| `is_allowed_by_robots_allows_when_no_robots_file` | `tools-mcp-webfetch/src/webfetch/http.rs:878` | 404 → allow all. |
| `is_allowed_by_robots_refuses_when_robots_server_errors` | `tools-mcp-webfetch/src/webfetch/http.rs:892` | 5xx → fail closed. |
| `is_allowed_by_robots_follows_same_origin_redirects_with_no_redirect_client` | `tools-mcp-webfetch/src/webfetch/http.rs:910` | robots redirect is followed even with redirects disabled on the caller's client. |
| `classify_webfetch_error_reports_robots_unavailable` | `tools-mcp-webfetch/src/webfetch_tool.rs:213` | Anyhow text matched into `robots_unavailable`. |
| `classify_webfetch_error_unknown_points_to_logs_not_details` | `tools-mcp-webfetch/src/webfetch_tool.rs:223` | `unknown` remediation guides operators to server logs, not to the details field. |
| `handle_webfetch_redacts_raw_network_error_details` | `tools-mcp-webfetch/src/webfetch_tool.rs:237` | Confirms `details` does not echo rejected hostnames or resolved addresses. |
| `chunk_markdown_*` | `tools-mcp-webfetch/src/webfetch/chunker.rs:404-658` | Token budget, heading boundaries, code-fence handling, UTF-8 safety. |
| `analyze_js_heavy *` tests | `tools-mcp-webfetch/src/webfetch/heuristics.rs:685-863` | Heuristic firing per indicator class. |
| `test_*` whitelist tests | `tools-mcp-webfetch/src/webfetch/whitelist.rs:146-179` | Exact, subdomain, wildcard, unparseable URL behavior. |

## 12. Open Questions

None.

## 13. Resolved Questions

| # | Question | Resolution |
|---|---|---|
| 1 | Does the documented "20s safety cap" exist as a single timeout? | No. The cap is composed of `NAVIGATION_TIMEOUT = 15s` wrapping the entire inner render call, which itself includes `wait_for_network_idle` bounded by `NETWORK_IDLE_MAX_WAIT = 5s` with idle declared after `NETWORK_IDLE_TIMEOUT = 2s` of inactivity. The total render budget is 15s, not 20s. (See §6.2 step 12.) |
| 2 | When `force_browser=true` and the browser path fails, does the tool fall back to HTTP? | No. Hard-fails with `error_type: browser_unavailable` (or another classified error). The fallback exists only when browser is attempted speculatively (whitelist match or heuristic). |
| 3 | Does the FetchResponse appear at the result top level? | No. It is serialized as a JSON string in `content[0].text`. Callers must parse that string. |

## 14. References

| Source | Use in this document |
|---|---|
| `tools-mcp-core/src/tool_outcome.rs` | `ToolCallOutcome::err_with` shape (§6.3) and `parse_args` deny-unknown-fields error wording (§6.1). |
| `tools-mcp-server/src/composition.rs` | `tools_mcp_webfetch::register_tools` is invoked at line 87 (§4.1). |
| `docs/security.md` | Project-wide trust-boundary guidance for untrusted external content (§7). |
| `docs/configuration.md` | Authoritative env-var catalog (§8). |
| `docs/tools-mcp-threat-model.md` | Adjacent threat model document (§7). |
